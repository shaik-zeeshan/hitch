//! `hitch-desktop` (src-tauri) — the thin Tauri client (ADR 0005).
//!
//! The GUI process holds no git/pty/store/agent logic: it only keeps a daemon
//! socket connection, starts the daemon when needed, and relays `hitch-proto`
//! requests/responses/events to Tauri IPC.

mod window_chrome;

use hitch_proto::transport::{connect_daemon as connect_transport, DaemonStream};
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
#[cfg(feature = "packaged-smoke")]
use std::time::{SystemTime, UNIX_EPOCH};
#[cfg(feature = "packaged-smoke")]
use std::{env, fs};
#[cfg(windows)]
use windows_sys::Win32::Foundation::{CloseHandle, ERROR_INVALID_PARAMETER};
#[cfg(windows)]
use windows_sys::Win32::System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE};

use hitch_core::SessionId;
#[cfg(feature = "packaged-smoke")]
use hitch_core::SessionParent;
use hitch_proto::{
    encode_control_message, encode_pty_frame, ControlMessage, ErrorCode, Event, ProtocolError,
    Request, RequestId, Response, PROTOCOL_VERSION,
};
use serde::Serialize;
use tauri::ipc::{Channel, InvokeResponseBody};
use tauri::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, State, WindowEvent, Wry};
#[cfg(feature = "packaged-smoke")]
use tauri::RunEvent;

/// Per-session bound on the bytes we stage before the webview registers that
/// session's output channel. Mirrors the daemon's `DEFAULT_SCROLLBACK_CAPACITY`
/// (1 MiB) so a brand-new session's first prompt survives the registration
/// round-trip without letting staging grow without bound (ADR 0007).
const OUTPUT_STAGING_CAPACITY: usize = 1024 * 1024;

/// How often the heartbeat thread pings the daemon, and how long it waits for a
/// `Pong` before declaring the daemon wedged (ADR 0009). The interval stays
/// generous: even though long git ops now run as Jobs off the request loop (ADR
/// 0008), a Ping shares the single connection with PTY input and other control
/// requests, so a tight timeout would false-positive under load.
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(15);
const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(20);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(2);
const HEARTBEAT_LOST_REASON: &str = "daemon stopped responding to heartbeat";
const DAEMON_RESPONSE_TIMEOUT_REASON: &str = "timed out waiting for daemon response";
const HANDSHAKE_FAILURE_PREFIX: &str = "daemon handshake failed: ";
const DAEMON_SHUTDOWN_GRACE: Duration = Duration::from_secs(5);
/// Restart-reason prefix marking a daemon the handshake found incompatible or
/// otherwise un-handshakeable. `should_force_kill_daemon` keys off this to
/// SIGKILL rather than negotiate a graceful shutdown.
const INCOMPATIBLE_DAEMON_PREFIX: &str = "restarting incompatible daemon";
/// How many times `connect_and_handshake` will restart-and-retry before giving
/// up. One retry is too brittle — a slow unbind or socket race on the first
/// restart would surface as a failure — but the crash-loop guard still backstops
/// a daemon that is genuinely unable to come up compatible.
const MAX_HANDSHAKE_ATTEMPTS: usize = 3;

/// Crash-loop guard window + cap. If the GUI has to (re)spawn the daemon more
/// than `CRASH_LOOP_MAX` times within `CRASH_LOOP_WINDOW`, it stops respawning
/// and surfaces `failed` + the log reason rather than thrashing (ADR 0009).
const CRASH_LOOP_MAX: usize = 4;
// Wide enough to span several `wait_for_daemon` timeouts (a daemon that never
// binds its socket fails ~10s per attempt), so a genuine crash loop trips before
// the budget rolls off.
const CRASH_LOOP_WINDOW: Duration = Duration::from_secs(60);

/// The Daemon Status the GUI surfaces, distinct from any single window's socket
/// link (`CONTEXT.md`, ADR 0009). Serialized kebab-case to match the frontend
/// `daemonStatus` store.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum DaemonStatus {
    /// Spawn issued, socket not yet up.
    Starting,
    /// A healthy daemon is listening and this GUI is attached + responsive.
    Running,
    /// No socket and no live daemon process found — it died or never ran.
    Unreachable,
    /// Spawn or startup errored; carries a reason sourced from the daemon log.
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecoveryMode {
    Reconnect,
    RestartDaemon,
}

/// Status snapshot pushed to the webview on every transition (`hitch-status`)
/// and returned by `get_daemon_status`.
#[derive(Debug, Clone, Serialize)]
struct StatusPayload {
    status: DaemonStatus,
    reason: Option<String>,
    log_path: String,
}

/// Bounded record of recent (re)spawn attempts, used to stop a crash-looping
/// daemon from thrashing (ADR 0009). Pure and clock-injected so the policy is
/// unit-testable without sleeping.
struct CrashLoopGuard {
    window: Duration,
    max_attempts: usize,
    attempts: Vec<Instant>,
}

impl CrashLoopGuard {
    fn new(max_attempts: usize, window: Duration) -> Self {
        Self {
            window,
            max_attempts,
            attempts: Vec::new(),
        }
    }

    /// Record a spawn attempt at `now`, dropping ones older than the window.
    /// Returns `true` while the count stays at or below the cap, `false` once the
    /// daemon has been respawned too many times in the window (crash-looping).
    fn allow(&mut self, now: Instant) -> bool {
        self.attempts
            .retain(|at| now.duration_since(*at) < self.window);
        self.attempts.push(now);
        self.attempts.len() <= self.max_attempts
    }

    /// Clear the record so only consecutive startup failures trip the guard.
    /// Called after a healthy handshake; explicit user restarts also clear it
    /// before recording their requested spawn.
    fn reset(&mut self) {
        self.attempts.clear();
    }
}

/// Read the last `lines` lines of the daemon log at `path`. Returns `None` when
/// the file is missing (the daemon never wrote one) or empty, so callers fall
/// back to a generic reason. Kept path-parameterized for unit testing.
fn read_log_tail(path: &Path, lines: usize) -> Option<String> {
    let contents = std::fs::read_to_string(path).ok()?;
    let trimmed = contents.trim_end_matches('\n');
    if trimmed.is_empty() {
        return None;
    }
    let all: Vec<&str> = trimmed.lines().collect();
    let start = all.len().saturating_sub(lines);
    Some(all[start..].join("\n"))
}

fn parse_spawned_daemon_pid(stdout: &[u8]) -> Option<u32> {
    std::str::from_utf8(stdout)
        .ok()?
        .split_whitespace()
        .rev()
        .find_map(|token| token.parse().ok())
}

fn run_detach_command(mut command: Command, label: &str) -> Result<u32, String> {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| format!("failed to spawn {label}: {err}"))?;

    let mut pid_line = String::new();
    if let Some(stdout) = child.stdout.as_mut() {
        BufReader::new(stdout)
            .read_line(&mut pid_line)
            .map_err(|err| format!("failed to read {label} daemon pid: {err}"))?;
    }

    let status = child
        .wait()
        .map_err(|err| format!("failed to wait for {label}: {err}"))?;

    if !status.success() {
        // The `--detach` launcher points the *real* daemon's stdio at the log
        // file (see `detach_spawn`), so its own stderr only carries failures
        // that happen *before* the daemon is spawned — bad args, a log-open
        // error, the spawn itself. Those never reach the daemon log, so surface
        // the launcher's stderr instead of an opaque exit status. Safe to drain
        // to EOF: the launcher is short-lived and the daemon does not inherit
        // this pipe, so there is no long-lived writer to block on.
        let mut stderr = String::new();
        if let Some(mut pipe) = child.stderr.take() {
            let _ = pipe.read_to_string(&mut stderr);
        }
        let stderr = stderr.trim();
        return Err(if stderr.is_empty() {
            format!("{label} failed with {status}")
        } else {
            format!("{label} failed with {status}: {stderr}")
        });
    }
    parse_spawned_daemon_pid(pid_line.as_bytes())
        .ok_or_else(|| format!("{label} did not print a daemon pid"))
}

fn recovery_mode_for_loss(reason: &str) -> RecoveryMode {
    if reason == HEARTBEAT_LOST_REASON || is_handshake_timeout(reason) {
        RecoveryMode::RestartDaemon
    } else {
        RecoveryMode::Reconnect
    }
}

fn is_handshake_timeout(reason: &str) -> bool {
    reason
        .strip_prefix(HANDSHAKE_FAILURE_PREFIX)
        .is_some_and(|inner| inner == DAEMON_RESPONSE_TIMEOUT_REASON)
}
fn should_force_kill_daemon(reason: &str) -> bool {
    // A wedged daemon (missed heartbeat), timed-out handshake, and incompatible
    // daemon are all untrustworthy to shut themselves down on request. Hard-kill
    // by a platform-safe process identity instead of asking nicely and waiting on
    // a graceful unbind that may never come.
    reason == HEARTBEAT_LOST_REASON
        || reason.starts_with(INCOMPATIBLE_DAEMON_PREFIX)
        || is_handshake_timeout(reason)
}
#[cfg(windows)]
fn windows_force_kill_pid_for_reason(reason: &str, cached_server_pid: Option<u32>) -> Option<u32> {
    should_force_kill_daemon(reason)
        .then_some(cached_server_pid)
        .flatten()
}

/// Render a failed `Hello` exchange into a human reason for the daemon status.
/// Every branch maps to the same outcome — force-restart — so this is purely the
/// message; collapsing them here keeps `connect_and_handshake` to one match arm.
fn describe_handshake_failure(outcome: &Result<Response, String>) -> String {
    match outcome {
        Ok(Response::Error { error }) => error.message.clone(),
        Ok(other) => format!("unexpected hello response: {other:?}"),
        Err(err) => err.clone(),
    }
}

/// Read the daemon's pid from the pidfile it writes beside the socket, but only
/// when a live daemon still holds the pidfile's advisory lock. This is the only
/// handle on a daemon we could not complete a `Hello` with (a protocol mismatch
/// returns no pid), so it's what makes force-killing such a daemon possible — but
/// a pidfile left by an unclean exit names a pid the OS may have since reused, and
/// SIGKILLing that would hit an unrelated process. The daemon holds the lock for
/// its whole lifetime (see `write_pidfile`), so a *free* lock means the writer is
/// gone and the pid is unsafe to target. Returns `None` if the file is absent,
/// unparsable, or stale (lock free).
#[cfg(unix)]
fn read_daemon_pidfile(socket_path: &Path) -> Option<u32> {
    let path = hitch_proto::transport::pidfile_path(socket_path);
    let pid: u32 = std::fs::read_to_string(&path).ok()?.trim().parse().ok()?;
    if pidfile_is_stale(&path) {
        return None;
    }
    Some(pid)
}

#[cfg(unix)]
fn daemon_pid_for_force_kill(socket_path: &Path, cached_pid: Option<u32>) -> Option<u32> {
    cached_pid.or_else(|| read_daemon_pidfile(socket_path))
}

#[cfg(windows)]
fn terminate_process(pid: u32) -> Result<(), String> {
    // SAFETY: Win32 process handle lifecycle is bounded to this function. The
    // handle is opened only with PROCESS_TERMINATE and is closed on every path
    // after a successful OpenProcess.
    let handle = unsafe { OpenProcess(PROCESS_TERMINATE, 0, pid) };
    if handle.is_null() {
        let err = io::Error::last_os_error();
        if err.raw_os_error() == Some(ERROR_INVALID_PARAMETER as i32) {
            return Ok(());
        }
        return Err(format!(
            "failed to open daemon process {pid} for termination: {err}"
        ));
    }

    // SAFETY: `handle` is a live process handle from OpenProcess. Exit code 1 is
    // arbitrary; the daemon is being force-terminated because graceful shutdown
    // cannot be trusted.
    let terminated = unsafe { TerminateProcess(handle, 1) } != 0;
    let terminate_err = if terminated {
        None
    } else {
        Some(io::Error::last_os_error())
    };
    // SAFETY: close the handle acquired above exactly once.
    unsafe {
        CloseHandle(handle);
    }
    if let Some(err) = terminate_err {
        return Err(format!("failed to terminate daemon process {pid}: {err}"));
    }
    Ok(())
}

/// Whether the pidfile's advisory lock is free — i.e. no live daemon holds it, so
/// the pid it names belongs to a dead (and possibly reused) process. We probe by
/// trying to take the lock non-blocking: success means it was free (stale), so we
/// release it again immediately and report stale. Any failure (lock held, or we
/// can't open the file) is treated as *not* stale, preserving the existing
/// recovery behaviour rather than risking a wedged daemon we can't kill.
#[cfg(unix)]
fn pidfile_is_stale(path: &Path) -> bool {
    use std::os::unix::io::AsRawFd;
    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    // SAFETY: `flock` on a valid fd; `LOCK_NB` so the probe never blocks.
    let acquired = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0;
    if acquired {
        // SAFETY: same valid fd we just locked; release so we don't hold it.
        unsafe {
            libc::flock(file.as_raw_fd(), libc::LOCK_UN);
        }
    }
    acquired
}

/// Outcome of `run_probe` other than a delivered value: the worker thread could
/// not be spawned, the wait timed out, or the worker exited without sending.
enum ProbeError {
    Spawn(io::Error),
    Timeout,
    Disconnected,
}

/// Tracks how many probe worker threads are still blocked after their caller
/// gave up waiting (i.e. timed out). A blocked thread holds its `work` closure
/// — typically a `connect_transport` call against a slow Windows pipe — and
/// cannot be cancelled, so it lingers until the underlying connect unblocks.
/// We don't try to kill it; we only refuse to pile on. Keyed by probe name so a
/// crash-looping recovery cycle cannot accumulate an unbounded fan of blocked
/// connect/hello/shutdown threads: at most one leaked worker per name may exist
/// at a time, and the next same-name probe waits for it to drain before
/// spawning a replacement.
static PROBE_INFLIGHT: Mutex<Option<HashMap<&'static str, mpsc::Receiver<()>>>> =
    Mutex::new(None);

/// Park the receiver half of a still-blocked probe worker so the next same-name
/// probe can detect it (and wait it out) instead of spawning a second thread.
fn park_inflight_probe(name: &'static str, done: mpsc::Receiver<()>) {
    if let Ok(mut guard) = PROBE_INFLIGHT.lock() {
        guard.get_or_insert_with(HashMap::new).insert(name, done);
    }
}

/// If a previous same-name probe worker is still blocked, take its completion
/// receiver so we can wait for it to drain. Returns `None` when no worker is
/// outstanding for `name`.
fn take_inflight_probe(name: &'static str) -> Option<mpsc::Receiver<()>> {
    PROBE_INFLIGHT
        .lock()
        .ok()
        .and_then(|mut guard| guard.as_mut().and_then(|map| map.remove(name)))
}

/// Run `work` on a named probe thread and wait up to `timeout` for its result.
/// Centralizes the spawn + channel + recv_timeout boilerplate shared by the
/// bounded connect / hello / shutdown probes; callers map `ProbeError` to their
/// own error type and messages.
///
/// On timeout the worker thread is abandoned (its `work` closure may still be
/// blocked in a slow connect). To keep such leaks from accumulating across
/// recovery loops, we park the abandoned worker's completion signal under
/// `name`: a later same-name probe first drains that parked worker (within its
/// own `timeout`) before spawning a fresh thread, so at most one leaked worker
/// per name can be outstanding at any moment.
fn run_probe<T, F>(name: &'static str, timeout: Duration, work: F) -> Result<T, ProbeError>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    // Drain any worker abandoned by a previous timed-out probe of this name
    // before spawning another, bounding leaked threads to one per name.
    if let Some(prev_done) = take_inflight_probe(name) {
        match prev_done.recv_timeout(timeout) {
            // Previous worker is still blocked: re-park it and refuse to spawn a
            // second thread for this name. The caller sees a timeout, exactly as
            // if its own probe had timed out.
            Err(mpsc::RecvTimeoutError::Timeout) => {
                park_inflight_probe(name, prev_done);
                return Err(ProbeError::Timeout);
            }
            // Worker finished (sent or dropped) — slot is free again.
            Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => {}
        }
    }

    let (tx, rx) = mpsc::channel();
    // A separate signal that fires (via drop) when the worker returns, even if
    // the result `tx` was already dropped; used to detect drain of a leaked
    // worker without depending on `T`.
    let (done_tx, done_rx) = mpsc::channel::<()>();
    thread::Builder::new()
        .name(name.to_string())
        .spawn(move || {
            let _done = done_tx;
            let _ = tx.send(work());
        })
        .map_err(ProbeError::Spawn)?;
    match rx.recv_timeout(timeout) {
        Ok(value) => Ok(value),
        Err(mpsc::RecvTimeoutError::Timeout) => {
            // Abandon the still-running worker but remember it so the next
            // same-name probe waits for it instead of stacking another thread.
            park_inflight_probe(name, done_rx);
            Err(ProbeError::Timeout)
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(ProbeError::Disconnected),
    }
}

fn connect_transport_bounded(path: &Path, timeout: Duration) -> io::Result<DaemonStream> {
    let display_path = path.to_path_buf();
    let conn_path = path.to_path_buf();
    match run_probe("hitch-daemon-connect-probe", timeout, move || {
        connect_transport(&conn_path)
    }) {
        Ok(result) => result,
        Err(ProbeError::Spawn(err)) => Err(io::Error::new(io::ErrorKind::Other, err)),
        Err(ProbeError::Timeout) => Err(io::Error::new(
            io::ErrorKind::TimedOut,
            format!(
                "timed out connecting to daemon at {}",
                display_path.display()
            ),
        )),
        Err(ProbeError::Disconnected) => Err(io::Error::new(
            io::ErrorKind::ConnectionAborted,
            "daemon connect probe exited without a result",
        )),
    }
}

fn wait_for_socket_release(path: &Path, timeout: Duration) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    loop {
        match connect_transport_bounded(path, Duration::from_millis(250)) {
            // Connect actively failed (NotFound / refused / aborted): the
            // endpoint is gone, so the socket has been released.
            Err(err) if err.kind() != io::ErrorKind::TimedOut => return Ok(()),
            // Either a live connection (`Ok`) or a timed-out probe. A timeout
            // does NOT mean release: a slow Windows daemon whose polling accept
            // loop hasn't serviced us yet is still alive, and treating it as
            // released would let a replacement daemon race the live one. Keep
            // waiting and re-probe until the endpoint truly disappears.
            Ok(_) | Err(_) => {}
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "refusing to spawn replacement daemon: {} is still accepting connections",
                path.display(),
            ));
        }
        thread::sleep(Duration::from_millis(100));
    }
}

/// Routes raw PTY bytes to the webview per session (ADR 0007).
/// Lives in the Tauri process. Channels are kept across an ordinary daemon
/// disconnect, but a `SessionOpened` replay invalidates that session's channel
/// until the webview has reset its byte ring and registered a fresh channel;
/// replay bytes are staged during that registration gap.
///
/// Channel type choice (verified against tauri 2.11.2): we store
/// `Channel<InvokeResponseBody>` and send `InvokeResponseBody::Raw(bytes)`. That
/// is the ONLY way to guarantee binary transmission on this version — the blanket
/// `impl<T: Serialize> IpcResponse for T` would route `&[u8]`/`Vec<u8]` through
/// serde as a JSON array of integers (`InvokeResponseBody::Json`, ~6x blowup).
/// `Raw` is delivered to JS as an ArrayBuffer (small payloads via
/// `new Uint8Array([...]).buffer`, large via the fetch-channel path), matching
/// the JS side `new Channel<ArrayBuffer>()`.
#[derive(Default)]
struct OutputRouter {
    channels: HashMap<SessionId, Channel<InvokeResponseBody>>,
    /// Bytes that arrived before the channel was registered, per session.
    staging: HashMap<SessionId, Vec<u8>>,
    /// Sessions that have already produced a SessionOpened event in this GUI
    /// process. A later SessionOpened for the same id is a reconnect/replay and
    /// should discard pre-replay staging; the first SessionOpened must preserve
    /// bytes that raced ahead of the event.
    opened_sessions: HashSet<SessionId>,
}

impl OutputRouter {
    /// Append `payload` to a session's bounded staging buffer, trimming the head
    /// on overflow so staging never exceeds `OUTPUT_STAGING_CAPACITY`.
    fn stage(&mut self, session_id: SessionId, payload: &[u8]) {
        let buf = self.staging.entry(session_id).or_default();
        buf.extend_from_slice(payload);
        if buf.len() > OUTPUT_STAGING_CAPACITY {
            let drop = buf.len() - OUTPUT_STAGING_CAPACITY;
            buf.drain(..drop);
        }
    }

    /// A session-opened event is followed by the daemon's authoritative
    /// scrollback replay on reconnect. Drop any old channel before the webview
    /// sees the event so replay cannot race through a stale channel and be wiped
    /// by the JS ring reset that happens during fresh registration. Preserve
    /// bytes staged before the first SessionOpened for a brand-new session: a PTY
    /// can emit its first prompt before the daemon broadcasts SessionOpened.
    fn prepare_fresh_registration(&mut self, session_id: SessionId) {
        let already_opened = !self.opened_sessions.insert(session_id);
        self.channels.remove(&session_id);
        if already_opened {
            self.staging.remove(&session_id);
        }
    }

    /// Route output to the active channel, or stage it until registration
    /// completes. Sending is best-effort: a dead webview channel should not tear
    /// down the daemon reader loop. If the channel has gone stale, remove it and
    /// stage the payload so the next registration can catch up.
    fn send_or_stage(&mut self, session_id: SessionId, payload: Vec<u8>) {
        if let Some(channel) = self.channels.get(&session_id) {
            if channel
                .send(InvokeResponseBody::Raw(payload.clone()))
                .is_err()
            {
                self.channels.remove(&session_id);
                self.stage(session_id, &payload);
            }
        } else {
            self.stage(session_id, &payload);
        }
    }

    /// Register (or re-register) a webview channel and flush bytes staged during
    /// the registration gap.
    fn register_channel(&mut self, session_id: SessionId, channel: Channel<InvokeResponseBody>) {
        if let Some(staged) = self.staging.remove(&session_id) {
            if !staged.is_empty()
                && channel
                    .send(InvokeResponseBody::Raw(staged.clone()))
                    .is_err()
            {
                self.channels.remove(&session_id);
                self.staging.insert(session_id, staged);
                return;
            }
        }
        self.channels.insert(session_id, channel);
    }

    /// Drop a session's output channel and any bytes staged for it.
    fn unregister_channel(&mut self, session_id: SessionId) {
        self.channels.remove(&session_id);
        self.staging.remove(&session_id);
    }

    /// Forget all routing state for a session that the daemon has closed.
    fn close_session(&mut self, session_id: SessionId) {
        self.unregister_channel(session_id);
        self.opened_sessions.remove(&session_id);
    }
}

#[derive(Clone)]
struct HitchClient(Arc<HitchClientInner>);

struct HitchClientInner {
    socket_path: PathBuf,
    next_request_id: AtomicU64,
    connected: AtomicBool,
    /// Bumped by attach_stream each time a new socket connection is established.
    /// Reader threads compare their captured generation before calling
    /// mark_disconnected, so a stale reader from an old connection cannot
    /// clobber a newer one (e.g. after a protocol-mismatch daemon restart).
    connection_generation: AtomicU64,
    connect_lock: Mutex<()>,
    writer: Mutex<Option<DaemonStream>>,
    pending: Mutex<HashMap<RequestId, mpsc::Sender<Response>>>,
    /// Live sessions, mirrored from daemon session-opened/closed events so the
    /// menu-bar tray can show how many sessions are still running.
    sessions: Mutex<HashSet<SessionId>>,
    /// The tray's status line; populated once the tray is built in `setup`.
    tray_status: Mutex<Option<MenuItem<Wry>>>,
    /// Per-session PTY-output channels + pre-registration staging (ADR 0007).
    /// One mutex guards both maps; the struct it protects is small.
    output_router: Mutex<OutputRouter>,
    /// The current four-state Daemon Status + its reason (ADR 0009). The tray and
    /// the `hitch-status` event both read this; `get_daemon_status` returns it.
    status: Mutex<DaemonStatus>,
    reason: Mutex<Option<String>>,
    /// Stops a crash-looping daemon from thrashing the respawn path (ADR 0009).
    restart_guard: Mutex<CrashLoopGuard>,
    /// Set while a recovery loop is in flight so a burst of disconnect signals
    /// (reader error + missed heartbeat) starts exactly one recovery.
    recovering: AtomicBool,
    /// Set while the user-requested restart path intentionally drops the old
    /// socket. EOF from that socket is expected and must not start auto-recovery.
    suppress_recovery: AtomicBool,
    /// Pid reported by the last successful Hello handshake. Recovery uses it to
    /// terminate a heartbeat-wedged daemon before waiting on endpoint release.
    daemon_pid: Mutex<Option<u32>>,
    /// Windows copy of the last successful Hello pid. Do not query named-pipe
    /// peer credentials during attach: on Windows that can block until the server
    /// accepts the pipe, which leaves the GUI stuck in `starting`.
    #[cfg(windows)]
    windows_daemon_pid: Mutex<Option<u32>>,
    /// Daemon log path, computed once to match the daemon writer.
    log_path: PathBuf,
    /// Ordered, fire-and-forget input lane (Slice 6, "input fast path"). Every
    /// keystroke is pushed here instead of riding the synchronous request path:
    /// a single drain thread (`spawn_input_drain`) owns the `Receiver`, writes
    /// one input frame per item to the shared `writer` IN ORDER, and never waits
    /// for the daemon's `Ack`. Funnelling all keystrokes through one consumer is
    /// what guarantees order — concurrent `spawn_blocking` writers could in
    /// principle interleave, this cannot. Best-effort by design: a write error
    /// or a disconnected socket drops the keystrokes silently (input flowing
    /// matters more than surfacing a transient socket error per character).
    input_tx: mpsc::Sender<(SessionId, Vec<u8>)>,
}

/// The tray's stable id, used to look it up for tooltip updates.
const TRAY_ID: &str = "hitch-tray";

fn connect_and_hello_over_new_connection(
    socket_path: &Path,
    request_id: RequestId,
    timeout: Duration,
) -> Result<(DaemonStream, BufReader<DaemonStream>, Response), String> {
    let socket_path = socket_path.to_path_buf();
    match run_probe("hitch-daemon-hello-probe", timeout, move || {
        (|| -> Result<(DaemonStream, BufReader<DaemonStream>, Response), String> {
            let mut stream = connect_transport(&socket_path)
                .map_err(|err| format!("failed to connect for daemon hello: {err}"))?;
            let mut reader = BufReader::new(
                stream
                    .try_clone()
                    .map_err(|err| format!("failed to clone daemon hello stream: {err}"))?,
            );
            let bytes = encode_control_message(&ControlMessage::request(
                request_id,
                Request::Hello {
                    client_name: "hitch-desktop".into(),
                    protocol_version: PROTOCOL_VERSION,
                },
            ))
            .map_err(|err| err.to_string())?;
            stream
                .write_all(&bytes)
                .map_err(|err| format!("failed to send daemon hello: {err}"))?;
            stream
                .flush()
                .map_err(|err| format!("failed to flush daemon hello: {err}"))?;

            loop {
                match read_control_message(&mut reader) {
                    Ok(Some(ControlMessage::Response { id, response })) if id == request_id => {
                        return Ok((stream, reader, response));
                    }
                    Ok(Some(_)) => {}
                    Ok(None) => {
                        return Err("daemon closed hello connection before response".to_string());
                    }
                    Err(err) => return Err(format!("failed to read daemon hello: {err}")),
                }
            }
        })()
    }) {
        Ok(result) => result,
        Err(ProbeError::Spawn(err)) => Err(format!("failed to spawn daemon hello probe: {err}")),
        Err(ProbeError::Timeout) => Err(DAEMON_RESPONSE_TIMEOUT_REASON.to_string()),
        Err(ProbeError::Disconnected) => {
            Err("daemon hello probe exited without a result".to_string())
        }
    }
}

impl HitchClient {
    fn new() -> Self {
        // The input lane is built up-front so `input_tx` can be a plain (non-Option)
        // field: the channel exists for the whole life of the client. The drain
        // thread needs a `HitchClient` handle to reach the shared `writer`, so we
        // build the inner first, wrap it, then spawn the drain thread with a clone
        // and hand it the `Receiver`.
        let (input_tx, input_rx) = mpsc::channel::<(SessionId, Vec<u8>)>();
        let client = Self(Arc::new(HitchClientInner {
            socket_path: hitch_proto::transport::default_socket_path(),
            next_request_id: AtomicU64::new(1),
            connected: AtomicBool::new(false),
            connection_generation: AtomicU64::new(0),
            connect_lock: Mutex::new(()),
            writer: Mutex::new(None),
            pending: Mutex::new(HashMap::new()),
            sessions: Mutex::new(HashSet::new()),
            tray_status: Mutex::new(None),
            output_router: Mutex::new(OutputRouter::default()),
            status: Mutex::new(DaemonStatus::Starting),
            reason: Mutex::new(None),
            restart_guard: Mutex::new(CrashLoopGuard::new(CRASH_LOOP_MAX, CRASH_LOOP_WINDOW)),
            recovering: AtomicBool::new(false),
            suppress_recovery: AtomicBool::new(false),
            daemon_pid: Mutex::new(None),
            #[cfg(windows)]
            windows_daemon_pid: Mutex::new(None),
            log_path: daemon_log_path(),
            input_tx,
        }));
        client.spawn_input_drain(input_rx);
        client
    }

    /// Spawn the single drain thread that owns the input lane (Slice 6). It
    /// `recv`s `(session_id, bytes)` items in the order they were enqueued and
    /// writes one PTY input frame per item to the daemon, never waiting for an
    /// `Ack`. Because exactly one thread consumes the channel and writes
    /// sequentially — locking the shared `writer` per frame — keystroke order is
    /// preserved end-to-end. The lock is re-taken each item (never held across
    /// items), so a reconnect that swaps the `writer` in `attach_stream` is
    /// picked up naturally on the next keystroke. The thread exits only when the
    /// `Sender` is dropped, i.e. when the client itself is gone.
    fn spawn_input_drain(&self, input_rx: mpsc::Receiver<(SessionId, Vec<u8>)>) {
        let client = self.clone();
        thread::Builder::new()
            .name("hitch-input-writer".into())
            .spawn(move || {
                while let Ok((session_id, bytes)) = input_rx.recv() {
                    client.write_input_frame(session_id, &bytes);
                }
            })
            .expect("failed to spawn input writer thread");
    }

    /// Write one fire-and-forget input frame to the daemon (Slice 6). Mirrors the
    /// frame shape `dispatch_request` writes (control message + PTY payload +
    /// flush) but deliberately does NOT register a pending-response channel and
    /// never waits: the daemon's `Ack` for this `request_id` is unmatched in the
    /// `pending` map and the reader silently drops it. Best-effort throughout — a
    /// missing writer (disconnected) or a write error just drops the keystrokes
    /// and returns. We do not call `handle_connection_lost` from here: that path
    /// re-enters the client and could deadlock against the heartbeat/reader, which
    /// already detect a dead socket; the next keystroke after reconnect succeeds.
    fn write_input_frame(&self, session_id: SessionId, bytes: &[u8]) {
        let Ok(frame) = encode_pty_frame(bytes) else {
            return;
        };

        // A fresh id is still required on the control message even though the
        // reply is ignored; pull it from the same monotonic counter as everyone
        // else so ids stay unique across the connection. Only allocate an id
        // after the payload is encodable: announcing a frame that cannot be sent
        // leaves the daemon waiting for bytes and corrupts the shared stream.
        let request_id = self.0.next_request_id.fetch_add(1, Ordering::SeqCst);
        let request = Request::SendSessionInput {
            session_id,
            byte_count: bytes.len() as u32,
        };

        let Ok(mut writer_guard) = self.0.writer.lock() else {
            return;
        };
        let Some(writer) = writer_guard.as_mut() else {
            // Disconnected: drop the keystroke. The reader/heartbeat own recovery.
            return;
        };
        let Ok(control) = encode_control_message(&ControlMessage::request(request_id, request))
        else {
            return;
        };
        // On any write/flush error, stop touching this frame and bail. The socket
        // is likely dead; leave teardown to the reader thread.
        if writer.write_all(&control).is_err() {
            return;
        }
        if writer.write_all(&frame).is_err() {
            return;
        }
        let _ = writer.flush();
    }

    /// Current Daemon Status snapshot for the tray, events, and `get_daemon_status`.
    fn status_payload(&self) -> StatusPayload {
        StatusPayload {
            status: self
                .0
                .status
                .lock()
                .map(|s| *s)
                .unwrap_or(DaemonStatus::Unreachable),
            reason: self.0.reason.lock().ok().and_then(|r| r.clone()),
            log_path: self.0.log_path.display().to_string(),
        }
    }

    /// Record the Daemon Status, refresh the tray, and push it to the webview.
    /// Every status transition flows through here so the indicator, tray, and
    /// `get_daemon_status` never disagree (ADR 0009).
    fn set_status(&self, app: &AppHandle, status: DaemonStatus, reason: Option<String>) {
        if let Ok(mut slot) = self.0.status.lock() {
            *slot = status;
        }
        if let Ok(mut slot) = self.0.reason.lock() {
            *slot = reason.clone();
        }
        self.refresh_tray(app);
        let _ = app.emit(
            "hitch-status",
            StatusPayload {
                status,
                reason,
                log_path: self.0.log_path.display().to_string(),
            },
        );
    }

    /// A concise failure reason sourced from the daemon log tail (ADR 0009): the
    /// last non-empty line (the panic/fatal message), else `None`.
    fn log_failure_reason(&self) -> Option<String> {
        read_log_tail(&self.0.log_path, 1)
    }
    fn set_daemon_pid(&self, daemon_pid: Option<u32>) {
        if let Ok(mut slot) = self.0.daemon_pid.lock() {
            *slot = daemon_pid;
        }
    }
    #[cfg(unix)]
    fn cached_daemon_pid(&self) -> Option<u32> {
        self.0.daemon_pid.lock().ok().and_then(|slot| *slot)
    }

    #[cfg(windows)]
    fn set_windows_daemon_pid(&self, pid: Option<u32>) {
        if let Ok(mut slot) = self.0.windows_daemon_pid.lock() {
            *slot = pid;
        }
    }

    #[cfg(windows)]
    fn cached_windows_daemon_pid(&self) -> Option<u32> {
        self.0.windows_daemon_pid.lock().ok().and_then(|slot| *slot)
    }

    #[cfg(unix)]
    fn force_kill_daemon(&self, preferred_pid: Option<u32>) -> Result<(), String> {
        // Heartbeat recovery has a trustworthy pid from the last successful Hello,
        // but `restart_daemon` must disconnect before it can respawn; pass that pid
        // in before `mark_disconnected` clears it. Protocol-mismatch restarts pass
        // `None` and use the pidfile instead, because a cached pid from an older
        // daemon could be stale while an upgraded daemon owns the socket.
        let daemon_pid = daemon_pid_for_force_kill(&self.0.socket_path, preferred_pid)
            .ok_or_else(|| "cannot force-restart daemon: daemon pid is unknown".to_string())?;
        unsafe {
            if libc::kill(daemon_pid as i32, libc::SIGKILL) != 0 {
                let err = io::Error::last_os_error();
                if err.raw_os_error() != Some(libc::ESRCH) {
                    return Err(format!("failed to kill wedged daemon {daemon_pid}: {err}"));
                }
            }
        }
        wait_for_socket_release(&self.0.socket_path, DAEMON_SHUTDOWN_GRACE)
    }

    #[cfg(windows)]
    fn force_kill_daemon(&self, preferred_pid: Option<u32>) -> Result<(), String> {
        let daemon_pid = preferred_pid
            .or_else(|| self.cached_windows_daemon_pid())
            .ok_or_else(|| {
                "cannot force-restart daemon: cached daemon pid is unknown".to_string()
            })?;
        terminate_process(daemon_pid)?;
        wait_for_socket_release(&self.0.socket_path, DAEMON_SHUTDOWN_GRACE)
    }

    fn record_startup_failure(&self, app: &AppHandle, err: String) -> String {
        self.set_status(app, DaemonStatus::Failed, Some(err.clone()));
        err
    }

    fn record_spawn_attempt(&self, app: &AppHandle) -> Result<(), String> {
        let now = Instant::now();
        let allowed = self
            .0
            .restart_guard
            .lock()
            .map(|mut guard| guard.allow(now))
            .unwrap_or(true);
        if allowed {
            return Ok(());
        }
        let reason = self
            .log_failure_reason()
            .unwrap_or_else(|| "daemon failed to start repeatedly; stopped retrying".to_string());
        self.set_status(app, DaemonStatus::Failed, Some(reason.clone()));
        Err(reason)
    }

    fn reset_restart_guard(&self) {
        if let Ok(mut guard) = self.0.restart_guard.lock() {
            guard.reset();
        }
    }

    fn mark_running(&self, app: &AppHandle) {
        self.reset_restart_guard();
        self.set_status(app, DaemonStatus::Running, None);
    }

    fn connect(&self, app: &AppHandle) -> Result<(), String> {
        if self.is_connected() {
            return Ok(());
        }

        let _guard = self
            .0
            .connect_lock
            .lock()
            .map_err(|_| "connection lock poisoned".to_string())?;
        if self.is_connected() {
            return Ok(());
        }
        let stream =
            match connect_transport_bounded(&self.0.socket_path, Duration::from_millis(500)) {
                Ok(stream) => stream,
                Err(err) if err.kind() == io::ErrorKind::TimedOut => {
                    return Err(format!("timed out connecting to live daemon: {err}"));
                }
                Err(_) => {
                    // Socket absent: we must (re)spawn. Guard against a crash loop —
                    // a daemon that dies on startup (corrupt store, bind failure)
                    // must not be respawned forever (ADR 0009).
                    self.record_spawn_attempt(app)?;
                    self.set_status(app, DaemonStatus::Starting, None);
                    self.spawn_daemon()
                        .map_err(|err| self.record_startup_failure(app, err))?;
                    self.wait_for_daemon()
                        .map_err(|err| self.record_startup_failure(app, err))?
                }
            };
        self.attach_stream(app, stream)
    }

    /// Connect (spawning if needed) and complete the `Hello` handshake,
    /// force-restarting an un-handshakeable daemon and retrying up to
    /// `MAX_HANDSHAKE_ATTEMPTS`. On success the Daemon Status becomes `running`
    /// and the crash-loop budget resets. Shared by the `connect_daemon` command
    /// and the auto-recovery loop so both diagnose failures identically.
    fn connect_and_handshake(&self, app: &AppHandle) -> Result<(), String> {
        let _guard = self
            .0
            .connect_lock
            .lock()
            .map_err(|_| "connection lock poisoned".to_string())?;
        if self.is_connected() {
            return Ok(());
        }

        let mut last_err = String::new();
        for attempt in 0..MAX_HANDSHAKE_ATTEMPTS {
            // Distinguish "endpoint absent" from "endpoint slow". A genuine
            // connect failure (NotFound / refused) means no daemon is listening,
            // so we must (re)spawn one. A *timeout* means the probe couldn't be
            // serviced in time — on Windows the daemon's polling accept loop can
            // be slow, so the daemon is likely alive. Respawning on timeout would
            // double-spawn a live daemon and burn crash-loop budget; instead we
            // fall through to the Hello attempt (with its own HANDSHAKE_TIMEOUT)
            // and let the slow accept complete.
            match connect_transport_bounded(&self.0.socket_path, Duration::from_millis(500)) {
                Ok(_) => {}
                Err(err) if err.kind() == io::ErrorKind::TimedOut => {}
                Err(_) => {
                    self.record_spawn_attempt(app)?;
                    self.set_status(app, DaemonStatus::Starting, None);
                    self.spawn_daemon()
                        .map_err(|err| self.record_startup_failure(app, err))?;
                    // Wait for the freshly spawned daemon to start accepting
                    // connections before attempting Hello; otherwise the not-yet-ready
                    // endpoint is mistaken for an incompatible daemon and force-killed,
                    // double-spawning on every cold start.
                    let _ = self
                        .wait_for_daemon()
                        .map_err(|err| self.record_startup_failure(app, err))?;
                }
            }

            let request_id = self.0.next_request_id.fetch_add(1, Ordering::SeqCst);
            match connect_and_hello_over_new_connection(
                &self.0.socket_path,
                request_id,
                HANDSHAKE_TIMEOUT,
            ) {
                Ok((stream, reader, Response::Hello { daemon_pid, .. })) => {
                    self.attach_with_reader(app, stream, reader)?;
                    self.set_daemon_pid(Some(daemon_pid));
                    #[cfg(windows)]
                    self.set_windows_daemon_pid(Some(daemon_pid));
                    self.mark_running(app);
                    return Ok(());
                }
                Ok((stream, reader, other)) => {
                    // A response has been received on this connection, so on
                    // Windows we may now query the pipe server pid without
                    // blocking (ADR 0012). Cache it so the upcoming
                    // `restart_daemon` can force-kill an incompatible daemon
                    // that never completed a Hello — otherwise the cached pid
                    // would be None and the force-restart would fail.
                    #[cfg(windows)]
                    if let Ok(pid) = stream.connected_pipe_server_pid() {
                        self.set_windows_daemon_pid(Some(pid));
                    }
                    let _ = &stream;
                    drop(reader);
                    last_err = describe_handshake_failure(&Ok(other));
                }
                Err(err) => {
                    last_err = err;
                }
            }
            if attempt + 1 == MAX_HANDSHAKE_ATTEMPTS {
                break;
            }
            self.restart_daemon(app, format!("{INCOMPATIBLE_DAEMON_PREFIX}: {last_err}"))?;
        }
        // In debug builds `spawn_daemon` execs prebuilt
        // `target/debug/hitch-daemon`/`hitch-hook` rather than rebuilding them
        // (see `spawn_daemon`). A protocol-mismatch handshake failure here almost
        // always means those binaries are stale relative to the desktop crate, and
        // restarting just respawns the same stale binary into a crash-loop. Surface
        // the rebuild fix in the reason so the dev isn't left guessing. Release
        // builds keep the daemon's own message untouched.
        #[cfg(debug_assertions)]
        if last_err.contains("protocol") {
            last_err.push_str(
                " (the debug hitch-daemon binary may be stale — rebuild with `cargo build -p hitch-daemon -p hitch-hook`)",
            );
        }
        if self.is_connected() {
            self.mark_disconnected(app, last_err.clone());
        }
        Err(last_err)
    }

    fn handshake_after_restart(&self, app: &AppHandle) -> Result<(), String> {
        match self.dispatch_request(
            app,
            Request::Hello {
                client_name: "hitch-desktop".into(),
                protocol_version: PROTOCOL_VERSION,
            },
            None,
            HANDSHAKE_TIMEOUT,
        ) {
            Ok(Response::Hello { daemon_pid, .. }) => {
                self.set_daemon_pid(Some(daemon_pid));
                #[cfg(windows)]
                self.set_windows_daemon_pid(Some(daemon_pid));
                self.mark_running(app);
                let _ = app.emit("hitch-reconnected", ());
                Ok(())
            }
            Ok(Response::Error { error }) => {
                let reason = format!("daemon hello failed after restart: {}", error.message);
                self.mark_disconnected(app, reason.clone());
                self.set_status(app, DaemonStatus::Unreachable, Some(reason.clone()));
                Err(reason)
            }
            Ok(other) => {
                let reason = format!("unexpected hello response after restart: {other:?}");
                self.mark_disconnected(app, reason.clone());
                self.set_status(app, DaemonStatus::Unreachable, Some(reason.clone()));
                Err(reason)
            }
            Err(err) => {
                let reason = format!("daemon hello failed after restart: {err}");
                self.mark_disconnected(app, reason.clone());
                self.set_status(app, DaemonStatus::Unreachable, Some(reason.clone()));
                Err(reason)
            }
        }
    }

    fn restart_daemon_and_handshake(&self, app: &AppHandle, reason: String) -> Result<(), String> {
        self.restart_daemon(app, reason)?;
        self.handshake_after_restart(app)
    }

    fn attach_stream(&self, app: &AppHandle, stream: DaemonStream) -> Result<(), String> {
        stream
            .set_nonblocking(false)
            .map_err(|err| format!("failed to configure daemon socket: {err}"))?;

        let writer = stream
            .try_clone()
            .map_err(|err| format!("failed to clone daemon socket: {err}"))?;
        *self
            .0
            .writer
            .lock()
            .map_err(|_| "writer lock poisoned".to_string())? = Some(writer);
        self.0.connected.store(true, Ordering::SeqCst);

        let generation = self.0.connection_generation.fetch_add(1, Ordering::SeqCst) + 1;
        let reader = BufReader::new(stream);
        self.start_reader(app.clone(), reader, generation);
        self.start_heartbeat(app.clone(), generation);
        Ok(())
    }

    /// Attach a connection whose `Hello` was already read by a probe, reusing
    /// the probe's `BufReader` rather than building a fresh one. The daemon
    /// enqueues `ReplayToClient` (scrollback + live output) immediately after
    /// the Hello response, so those frames can already be buffered inside the
    /// probe's reader. Building a new reader here would drop the probe's buffer
    /// and lose (or desync on) those frames; carrying the same reader forward
    /// preserves every byte read past the Hello frame. `writer_stream` is the
    /// writer half; `reader` already wraps a separate clone of the connection,
    /// so no extra clone is needed.
    fn attach_with_reader(
        &self,
        app: &AppHandle,
        writer_stream: DaemonStream,
        reader: BufReader<DaemonStream>,
    ) -> Result<(), String> {
        writer_stream
            .set_nonblocking(false)
            .map_err(|err| format!("failed to configure daemon socket: {err}"))?;

        *self
            .0
            .writer
            .lock()
            .map_err(|_| "writer lock poisoned".to_string())? = Some(writer_stream);
        self.0.connected.store(true, Ordering::SeqCst);

        let generation = self.0.connection_generation.fetch_add(1, Ordering::SeqCst) + 1;
        self.start_reader(app.clone(), reader, generation);
        self.start_heartbeat(app.clone(), generation);
        Ok(())
    }

    /// Spawn the `Ping`/`Pong` heartbeat for this connection (ADR 0009). It makes
    /// the `running` status mean *responsive*, not merely socket-open: a wedged
    /// daemon answers no Pong, so a timed-out Ping triggers recovery. Guarded by
    /// `connection_generation` like the reader, so a heartbeat from a superseded
    /// connection can neither fire recovery nor outlive its socket.
    fn start_heartbeat(&self, app: AppHandle, generation: u64) {
        let client = self.clone();
        thread::Builder::new()
            .name("hitch-daemon-heartbeat".into())
            .spawn(move || loop {
                thread::sleep(HEARTBEAT_INTERVAL);
                if client.0.connection_generation.load(Ordering::SeqCst) != generation {
                    return;
                }
                if !client.is_connected() {
                    return;
                }
                // Bypass `connect()` (no respawn from a heartbeat) — dispatch the
                // Ping directly with a tolerant timeout.
                let pong = client.dispatch_request(&app, Request::Ping, None, HEARTBEAT_TIMEOUT);
                if client.0.connection_generation.load(Ordering::SeqCst) != generation {
                    return;
                }
                if !matches!(pong, Ok(Response::Pong)) {
                    client.handle_connection_lost(&app, HEARTBEAT_LOST_REASON);
                    return;
                }
            })
            .expect("failed to spawn daemon heartbeat thread");
    }

    fn restart_daemon(&self, app: &AppHandle, reason: String) -> Result<(), String> {
        self.0.suppress_recovery.store(true, Ordering::SeqCst);
        let result = (|| {
            // Always ask first: ShutdownDaemon is a stable unit message even an
            // older/incompatible daemon understands, so it's the one teardown that
            // works across protocol versions.
            self.request_daemon_shutdown();
            let force_kill = should_force_kill_daemon(&reason);
            #[cfg(unix)]
            let force_kill_pid = if reason == HEARTBEAT_LOST_REASON {
                self.cached_daemon_pid()
            } else {
                None
            };
            #[cfg(windows)]
            let force_kill_pid =
                windows_force_kill_pid_for_reason(&reason, self.cached_windows_daemon_pid());
            self.mark_disconnected(app, reason.clone());
            self.record_spawn_attempt(app)?;
            self.set_status(app, DaemonStatus::Starting, None);

            // Wedged daemons are torn down with the platform's non-cooperative
            // kill primitive: Unix uses a verified pidfile/Hello pid, Windows uses
            // the last successful Hello pid. In every successful case we then wait
            // for the endpoint to stop accepting connections so we never spawn
            // over a live daemon.
            if force_kill {
                let kill_result = self.force_kill_daemon(force_kill_pid);
                #[cfg(unix)]
                {
                    let _ = kill_result;
                }
                #[cfg(windows)]
                if let Err(err) = kill_result {
                    self.set_status(app, DaemonStatus::Failed, Some(err.clone()));
                    return Err(err);
                }
            }
            wait_for_socket_release(&self.0.socket_path, DAEMON_SHUTDOWN_GRACE)?;

            self.spawn_daemon()?;
            let stream = self.wait_for_daemon()?;
            self.attach_stream(app, stream)
        })();
        self.0.suppress_recovery.store(false, Ordering::SeqCst);
        result
    }

    fn send_request(&self, app: &AppHandle, request: Request) -> Result<Response, String> {
        self.send_request_with_payload(app, request, None)
    }

    fn send_request_with_payload(
        &self,
        app: &AppHandle,
        request: Request,
        pty_payload: Option<Vec<u8>>,
    ) -> Result<Response, String> {
        self.connect(app)?;
        // Fixed client-side response deadline. With long ops now running as Jobs
        // (the `StartJob` reply is immediate and the real result rides a
        // `JobCompleted` event), no synchronous request should approach this; it
        // remains a backstop against a wedged daemon.
        self.dispatch_request(app, request, pty_payload, Duration::from_secs(120))
    }

    /// Write a request and block for its response up to `timeout`, without
    /// auto-connecting. The heartbeat uses this directly (a missed Pong must not
    /// trigger a respawn from inside the heartbeat); `send_request_with_payload`
    /// wraps it with `connect`.
    fn dispatch_request(
        &self,
        app: &AppHandle,
        request: Request,
        pty_payload: Option<Vec<u8>>,
        timeout: Duration,
    ) -> Result<Response, String> {
        let request_id = self.0.next_request_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = mpsc::channel();
        self.0
            .pending
            .lock()
            .map_err(|_| "pending-response lock poisoned".to_string())?
            .insert(request_id, tx);

        let write_result = (|| -> Result<(), String> {
            let mut writer_guard = self
                .0
                .writer
                .lock()
                .map_err(|_| "writer lock poisoned".to_string())?;
            let writer = writer_guard
                .as_mut()
                .ok_or_else(|| "daemon socket is not connected".to_string())?;
            let control = encode_control_message(&ControlMessage::request(request_id, request))
                .map_err(|err| err.to_string())?;
            writer
                .write_all(&control)
                .map_err(|err| format!("failed to send daemon request: {err}"))?;
            if let Some(payload) = pty_payload.as_deref() {
                let frame = encode_pty_frame(payload).map_err(|err| err.to_string())?;
                writer
                    .write_all(&frame)
                    .map_err(|err| format!("failed to send PTY input: {err}"))?;
            }
            writer
                .flush()
                .map_err(|err| format!("failed to flush daemon request: {err}"))?;
            Ok(())
        })();

        if let Err(err) = write_result {
            self.remove_pending(request_id);
            self.handle_connection_lost(app, &err);
            return Err(err);
        }

        match rx.recv_timeout(timeout) {
            Ok(response) => Ok(response),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                self.remove_pending(request_id);
                Err(DAEMON_RESPONSE_TIMEOUT_REASON.into())
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                Err("daemon response channel disconnected".into())
            }
        }
    }

    fn is_connected(&self) -> bool {
        self.0.connected.load(Ordering::SeqCst)
            && self
                .0
                .writer
                .lock()
                .map(|writer| writer.is_some())
                .unwrap_or(false)
    }

    fn remove_pending(&self, request_id: RequestId) {
        if let Ok(mut pending) = self.0.pending.lock() {
            pending.remove(&request_id);
        }
    }

    fn start_reader(&self, app: AppHandle, reader: BufReader<DaemonStream>, generation: u64) {
        let client = self.clone();
        thread::Builder::new()
            .name("hitch-daemon-reader".into())
            .spawn(move || {
                let result = reader_loop(&app, &client, reader);
                if let Err(err) = result {
                    // Only disconnect if this reader still owns the active connection.
                    // A stale reader from a superseded connection must not clobber
                    // the writer and pending map of the replacement connection.
                    if client.0.connection_generation.load(Ordering::SeqCst) == generation {
                        client.handle_connection_lost(&app, &err.to_string());
                    }
                }
            })
            .expect("failed to spawn daemon reader thread");
    }

    fn clear_connection_state(&self, reason: &str, drop_writer: bool) {
        self.0.connected.store(false, Ordering::SeqCst);
        // The cached Hello pid is only meaningful while we're handshook with that
        // exact daemon. Once disconnected it's stale — a daemon swapped underneath
        // us (upgrade, crash+respawn) could leave the old pid reused by another
        // process. Clear it instead of risking an unrelated kill.
        if let Ok(mut slot) = self.0.daemon_pid.lock() {
            *slot = None;
        }
        #[cfg(windows)]
        if drop_writer {
            self.set_windows_daemon_pid(None);
        }
        if drop_writer {
            if let Ok(mut writer) = self.0.writer.lock() {
                *writer = None;
            }
        }
        if let Ok(mut sessions) = self.0.sessions.lock() {
            sessions.clear();
        }

        let error_response = Response::Error {
            error: ProtocolError::new(ErrorCode::Unavailable, reason.to_string()).retryable(true),
        };
        if let Ok(mut pending) = self.0.pending.lock() {
            for (_, tx) in pending.drain() {
                let _ = tx.send(error_response.clone());
            }
        }
    }

    fn mark_disconnected(&self, app: &AppHandle, reason: String) {
        self.clear_connection_state(&reason, true);
        self.refresh_tray(app);
        let _ = app.emit("hitch-disconnected", DisconnectedPayload { reason });
    }

    /// Handle an *unexpected* loss of the daemon link (reader EOF/error, missed
    /// heartbeat, or a failed write). Marks the connection unavailable, surfaces
    /// the loss as a Daemon Status with a log-sourced reason, then kicks off
    /// bounded auto-recovery (ADR 0009). The deliberate restart path
    /// (`restart_daemon`) does NOT route through here — it manages its own reconnect.
    fn handle_connection_lost(&self, app: &AppHandle, reason: &str) {
        if self.0.suppress_recovery.load(Ordering::SeqCst) {
            return;
        }
        let mode = recovery_mode_for_loss(reason);
        let reason = reason.to_string();
        self.clear_connection_state(&reason, mode != RecoveryMode::RestartDaemon);
        self.refresh_tray(app);
        let _ = app.emit(
            "hitch-disconnected",
            DisconnectedPayload {
                reason: reason.clone(),
            },
        );
        // Socket-absent reads as `unreachable`; recovery refines this to
        // `starting` while retrying and `failed` if the crash-loop guard trips.
        self.set_status(
            app,
            DaemonStatus::Unreachable,
            self.log_failure_reason().or_else(|| Some(reason.clone())),
        );
        self.begin_recovery(app, mode);
    }

    /// Start the auto-recovery loop unless one is already running. The
    /// `recovering` latch collapses a burst of disconnect signals (reader error
    /// arriving alongside a missed heartbeat) into a single recovery.
    fn begin_recovery(&self, app: &AppHandle, mode: RecoveryMode) {
        if self.0.recovering.swap(true, Ordering::SeqCst) {
            return;
        }
        let client = self.clone();
        let app = app.clone();
        thread::Builder::new()
            .name("hitch-daemon-recovery".into())
            .spawn(move || {
                client.recovery_loop(&app, mode);
                client.0.recovering.store(false, Ordering::SeqCst);
            })
            .expect("failed to spawn daemon recovery thread");
    }

    /// Reconnect/restart with exponential backoff until the daemon is healthy
    /// again or the crash-loop guard gives up and sets `failed`. On success the
    /// webview is told to re-snapshot; sessions replay through the daemon's
    /// normal reconnect events (ADR 0007).
    fn recovery_loop(&self, app: &AppHandle, mode: RecoveryMode) {
        let mut delay = Duration::from_millis(300);
        let max_delay = Duration::from_secs(5);
        loop {
            if self.is_connected()
                && matches!(
                    self.0.status.lock().map(|status| *status),
                    Ok(DaemonStatus::Running)
                )
            {
                return;
            }
            let result = match mode {
                RecoveryMode::Reconnect => self.connect_and_handshake(app),
                RecoveryMode::RestartDaemon => {
                    self.restart_daemon_and_handshake(app, HEARTBEAT_LOST_REASON.to_string())
                }
            };
            match result {
                Ok(()) => {
                    if mode == RecoveryMode::Reconnect {
                        let _ = app.emit("hitch-reconnected", ());
                    }
                    return;
                }
                Err(_) => {
                    // The spawn paths set `failed` when the crash-loop guard
                    // trips; stop retrying then rather than thrash.
                    if matches!(
                        self.0.status.lock().map(|status| *status),
                        Ok(DaemonStatus::Failed)
                    ) {
                        return;
                    }
                    thread::sleep(delay);
                    delay = (delay * 2).min(max_delay);
                }
            }
        }
    }

    /// Mirror a session's liveness from a daemon event and refresh the tray.
    fn track_session(&self, app: &AppHandle, session_id: SessionId, alive: bool) {
        let changed = match self.0.sessions.lock() {
            Ok(mut sessions) => {
                if alive {
                    sessions.insert(session_id)
                } else {
                    sessions.remove(&session_id)
                }
            }
            Err(_) => false,
        };
        if changed {
            self.refresh_tray(app);
        }
    }

    /// Update the tray's status line + tooltip with the current session count.
    /// All tray mutation happens on the main thread, where the menu lib is safe.
    fn refresh_tray(&self, app: &AppHandle) {
        let count = self.0.sessions.lock().map(|set| set.len()).unwrap_or(0);
        let status = self
            .0
            .status
            .lock()
            .map(|s| *s)
            .unwrap_or(DaemonStatus::Unreachable);
        let text = tray_status_text(status, count);
        let status_item = self
            .0
            .tray_status
            .lock()
            .ok()
            .and_then(|guard| guard.clone());
        let app = app.clone();
        let _ = app.clone().run_on_main_thread(move || {
            if let Some(item) = status_item {
                let _ = item.set_text(&text);
            }
            if let Some(tray) = app.tray_by_id(TRAY_ID) {
                let _ = tray.set_tooltip(Some(text.as_str()));
            }
        });
    }

    fn request_daemon_shutdown_over_new_connection(
        socket_path: &Path,
        request_id: RequestId,
    ) -> Result<(), String> {
        let socket_path = socket_path.to_path_buf();
        match run_probe(
            "hitch-daemon-shutdown-probe",
            Duration::from_secs(2),
            move || {
                (|| -> Result<(), String> {
                    let mut stream = connect_transport(&socket_path)
                        .map_err(|err| format!("failed to connect for daemon shutdown: {err}"))?;
                    let bytes = encode_control_message(&ControlMessage::request(
                        request_id,
                        Request::ShutdownDaemon,
                    ))
                    .map_err(|err| err.to_string())?;
                    stream
                        .write_all(&bytes)
                        .map_err(|err| format!("failed to send daemon shutdown: {err}"))?;
                    stream
                        .flush()
                        .map_err(|err| format!("failed to flush daemon shutdown: {err}"))?;

                    let mut reader = BufReader::new(stream);
                    match read_control_message(&mut reader) {
                        Ok(Some(ControlMessage::Response {
                            id,
                            response: Response::Ack,
                        })) if id == request_id => Ok(()),
                        Ok(Some(other)) => {
                            Err(format!("unexpected daemon shutdown response: {other:?}"))
                        }
                        Ok(None) => Err("daemon closed shutdown connection before ack".to_string()),
                        Err(err) => Err(format!("failed to read daemon shutdown ack: {err}")),
                    }
                })()
            },
        ) {
            Ok(result) => result,
            Err(ProbeError::Spawn(err)) => {
                Err(format!("failed to spawn daemon shutdown probe: {err}"))
            }
            Err(ProbeError::Timeout) => {
                Err("timed out waiting for daemon shutdown ack".to_string())
            }
            Err(ProbeError::Disconnected) => {
                Err("daemon shutdown probe exited without a result".to_string())
            }
        }
    }

    /// Ask the daemon to shut down. When this client has not completed startup
    /// yet, there may be no persistent writer even though the daemon endpoint is
    /// live; in that case use a short-lived connection and wait for Ack so manual
    /// restart can actually release the pipe before spawning.
    fn request_daemon_shutdown(&self) -> bool {
        let request_id = self.0.next_request_id.fetch_add(1, Ordering::SeqCst);
        let Ok(bytes) = encode_control_message(&ControlMessage::request(
            request_id,
            Request::ShutdownDaemon,
        )) else {
            return false;
        };
        if let Ok(mut guard) = self.0.writer.lock() {
            if let Some(writer) = guard.as_mut() {
                if writer.write_all(&bytes).is_ok() && writer.flush().is_ok() {
                    return true;
                }
            }
        }
        Self::request_daemon_shutdown_over_new_connection(&self.0.socket_path, request_id).is_ok()
    }

    fn spawn_daemon(&self) -> Result<(), String> {
        // In debug builds, `cargo tauri dev` already runs the package-level dev
        // script, which builds hitch-daemon and hitch-hook before launching the
        // desktop. Do not run nested `cargo` here: the GUI itself is already
        // under `cargo run`, and waiting on another cargo child during startup
        // leaves daemon status stuck at `starting`.
        #[cfg(not(debug_assertions))]
        if let Some(path) = daemon_binary_path() {
            let hook_path = hook_binary_path_for_daemon(&path).ok_or_else(|| {
                format!(
                    "hitch-hook binary was not found next to {}; bundled agent hooks cannot be installed",
                    path.display()
                )
            })?;
            let mut command = Command::new(&path);
            command
                .arg("--socket")
                .arg(&self.0.socket_path)
                .arg("--hook-helper")
                .arg(&hook_path)
                .arg("--detach");
            let pid = run_detach_command(command, &path.display().to_string())?;
            self.set_daemon_pid(Some(pid));
            #[cfg(windows)]
            self.set_windows_daemon_pid(Some(pid));
            return Ok(());
        }

        #[cfg(debug_assertions)]
        {
            let exe_dir = std::env::current_exe()
                .ok()
                .and_then(|path| path.parent().map(Path::to_path_buf))
                .ok_or_else(|| "failed to locate debug binary directory".to_string())?;
            let daemon_path = exe_dir.join(debug_binary_name("hitch-daemon"));
            if !daemon_path.is_file() {
                return Err(format!(
                    "{} was not found; run `cargo build -p hitch-daemon -p hitch-hook`",
                    daemon_path.display()
                ));
            }
            let hook_path = exe_dir.join(debug_binary_name("hitch-hook"));
            if !hook_path.is_file() {
                return Err(format!(
                    "{} was not found; run `cargo build -p hitch-daemon -p hitch-hook`",
                    hook_path.display()
                ));
            }

            let mut command = Command::new(&daemon_path);
            command
                .arg("--socket")
                .arg(&self.0.socket_path)
                .arg("--hook-helper")
                .arg(&hook_path)
                .arg("--detach");
            let pid = run_detach_command(command, &daemon_path.display().to_string())?;
            self.set_daemon_pid(Some(pid));
            #[cfg(windows)]
            self.set_windows_daemon_pid(Some(pid));
            return Ok(());
        }

        #[cfg(not(debug_assertions))]
        Err("hitch-daemon binary was not found next to the app; set HITCH_DAEMON_PATH".into())
    }

    fn wait_for_daemon(&self) -> Result<DaemonStream, String> {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut last_error = None;
        while Instant::now() < deadline {
            match connect_transport_bounded(&self.0.socket_path, Duration::from_millis(500)) {
                Ok(stream) => return Ok(stream),
                Err(err) => {
                    last_error = Some(err);
                    thread::sleep(Duration::from_millis(100));
                }
            }
        }
        // The socket never came up. The reason almost always lives in the
        // daemon's own log (a startup panic, a bind/store error) — surface its
        // tail so the user sees *why* instead of a bare timeout (ADR 0009).
        let connect_err = last_error
            .map(|err| err.to_string())
            .unwrap_or_else(|| "unknown error".into());
        match read_log_tail(&self.0.log_path, 3) {
            Some(tail) => Err(format!(
                "daemon did not become ready at {}: {connect_err}\n{tail}",
                self.0.socket_path.display(),
            )),
            None => Err(format!(
                "daemon did not become ready at {}: {connect_err}",
                self.0.socket_path.display(),
            )),
        }
    }
}

fn reader_loop(
    app: &AppHandle,
    client: &HitchClient,
    mut reader: BufReader<DaemonStream>,
) -> io::Result<()> {
    loop {
        let Some(message) = read_control_message(&mut reader)? else {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "daemon socket closed",
            ));
        };

        match message {
            ControlMessage::Response { id, response } => {
                if let Ok(mut pending) = client.0.pending.lock() {
                    if let Some(tx) = pending.remove(&id) {
                        let _ = tx.send(response);
                    }
                }
            }
            ControlMessage::Event { event } => {
                match &event {
                    Event::SessionOutput {
                        session_id,
                        byte_count,
                    } => {
                        let payload = read_pty_payload(&mut reader)?;
                        if payload.len() != *byte_count as usize {
                            return Err(io::Error::new(
                                io::ErrorKind::InvalidData,
                                format!(
                                    "PTY payload length {} did not match announced byte_count {byte_count}",
                                    payload.len()
                                ),
                            ));
                        }
                        // Stream RAW bytes to the webview over the per-session
                        // binary Channel — never stringify here (ADR 0007). If
                        // the channel isn't registered yet (registration is a
                        // round-trip the daemon may beat), stage the bytes so
                        // the first prompt/reconnect replay isn't lost.
                        if let Ok(mut router) = client.0.output_router.lock() {
                            router.send_or_stage(*session_id, payload);
                        }
                        continue;
                    }
                    Event::SessionOpened { session, .. } => {
                        if let Ok(mut router) = client.0.output_router.lock() {
                            router.prepare_fresh_registration(session.id);
                        }
                        client.track_session(app, session.id, true)
                    }
                    Event::SessionClosed { session_id, .. } => {
                        if let Ok(mut router) = client.0.output_router.lock() {
                            router.close_session(*session_id);
                        }
                        client.track_session(app, *session_id, false)
                    }
                    _ => {}
                }
                app.emit("hitch-event", event).map_err(io::Error::other)?;
            }
            ControlMessage::Request { .. } => {
                // The daemon should never send requests to the desktop client.
            }
        }
    }
}

fn read_control_message<R: BufRead>(reader: &mut R) -> io::Result<Option<ControlMessage>> {
    let mut line = Vec::new();
    let len = reader.read_until(b'\n', &mut line)?;
    if len == 0 {
        return Ok(None);
    }
    if line.last() == Some(&b'\n') {
        line.pop();
    }
    if line.is_empty() {
        return Ok(None);
    }
    serde_json::from_slice(&line)
        .map(Some)
        .map_err(io::Error::other)
}

fn read_pty_payload<R: Read>(reader: &mut R) -> io::Result<Vec<u8>> {
    let mut prefix = [0_u8; 4];
    reader.read_exact(&mut prefix)?;
    let len = u32::from_be_bytes(prefix) as usize;
    let mut payload = vec![0_u8; len];
    reader.read_exact(&mut payload)?;
    Ok(payload)
}

#[cfg(not(debug_assertions))]
fn daemon_binary_path() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("HITCH_DAEMON_PATH") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Some(path);
        }
    }

    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    let plain = dir.join(daemon_plain_name());
    if plain.is_file() {
        return Some(plain);
    }
    if let Some(name) = sidecar_daemon_name() {
        let sidecar = dir.join(name);
        if sidecar.is_file() {
            return Some(sidecar);
        }
    }

    None
}

#[cfg(not(debug_assertions))]
fn daemon_plain_name() -> &'static str {
    if cfg!(windows) {
        "hitch-daemon.exe"
    } else {
        "hitch-daemon"
    }
}

#[cfg(debug_assertions)]
fn debug_binary_name(stem: &str) -> String {
    if cfg!(windows) {
        format!("{stem}.exe")
    } else {
        stem.to_string()
    }
}

#[cfg(not(debug_assertions))]
fn sidecar_daemon_name() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => Some("hitch-daemon-x86_64-pc-windows-msvc.exe"),
        ("windows", "aarch64") => Some("hitch-daemon-aarch64-pc-windows-msvc.exe"),
        ("macos", "x86_64") => Some("hitch-daemon-x86_64-apple-darwin"),
        ("macos", "aarch64") => Some("hitch-daemon-aarch64-apple-darwin"),
        ("linux", "x86_64") => Some("hitch-daemon-x86_64-unknown-linux-gnu"),
        ("linux", "aarch64") => Some("hitch-daemon-aarch64-unknown-linux-gnu"),
        _ => None,
    }
}

#[cfg(any(not(debug_assertions), test))]
fn hook_binary_path_for_daemon(daemon: &Path) -> Option<PathBuf> {
    let dir = daemon.parent()?;
    let file_name = daemon.file_name()?.to_str()?;
    let suffix = file_name.strip_prefix("hitch-daemon").unwrap_or_default();
    let hook = dir.join(format!("hitch-hook{suffix}"));
    hook.is_file().then_some(hook)
}

#[derive(Debug, Clone, Serialize)]
struct DisconnectedPayload {
    reason: String,
}

/// Path to the daemon's log. MUST match the daemon's own `daemon_log_path` so
/// the GUI tails the file the daemon writes — never derived from the endpoint
/// parent, to avoid drift. Unix keeps the existing `$HOME/.hitch*` layout;
/// Windows uses the transport-owned `%LOCALAPPDATA%\Hitch\<namespace>` layout.
#[cfg(windows)]
fn daemon_log_path() -> PathBuf {
    hitch_proto::transport::default_data_dir().join("daemon.log")
}

#[cfg(not(windows))]
fn daemon_log_path() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join(hitch_proto::transport::instance_dir_name())
        .join("daemon.log")
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct EditorLaunchSpec {
    program: OsString,
    args: Vec<OsString>,
    #[cfg(windows)]
    command_processor_shim: bool,
}

impl EditorLaunchSpec {
    fn new(program: impl Into<OsString>, path: &Path) -> Self {
        Self {
            program: program.into(),
            args: vec![path.as_os_str().to_os_string()],
            #[cfg(windows)]
            command_processor_shim: false,
        }
    }

    #[cfg(windows)]
    fn command_shim(program: impl Into<OsString>, path: &Path) -> Self {
        Self {
            program: program.into(),
            args: vec![path.as_os_str().to_os_string()],
            command_processor_shim: true,
        }
    }
}

fn trim_configured_editor(editor: &str) -> &str {
    let editor = editor.trim();
    let unquoted = editor
        .strip_prefix('"')
        .and_then(|inner| inner.strip_suffix('"'))
        .or_else(|| {
            editor
                .strip_prefix('\'')
                .and_then(|inner| inner.strip_suffix('\''))
        });
    unquoted.map(str::trim).unwrap_or(editor)
}

fn configured_executable(editor: &str) -> Option<PathBuf> {
    let path = PathBuf::from(editor);
    path.is_file().then_some(path)
}

#[cfg(windows)]
fn normalized_editor_name(editor: &str) -> String {
    editor
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

#[cfg(windows)]
fn push_editor_candidate(candidates: &mut Vec<OsString>, base: Option<&Path>, relative: &str) {
    if let Some(base) = base {
        candidates.push(base.join(relative).into_os_string());
    }
}

#[cfg(windows)]
fn push_program_files_candidates(
    candidates: &mut Vec<OsString>,
    program_files: Option<&Path>,
    program_files_x86: Option<&Path>,
    relative: &str,
) {
    push_editor_candidate(candidates, program_files, relative);
    push_editor_candidate(candidates, program_files_x86, relative);
}

#[cfg(windows)]
fn windows_editor_candidates_from_dirs(
    editor: &str,
    local_app_data: Option<&Path>,
    program_files: Option<&Path>,
    program_files_x86: Option<&Path>,
) -> Vec<OsString> {
    let mut candidates = Vec::new();

    let normalized = match (normalized_editor_name(editor), editor.contains("++")) {
        (name, true) if name == "notepad" => "notepadplusplus".to_string(),
        (name, _) => name,
    };
    match normalized.as_str() {
        "code" | "vscode" | "visualstudiocode" => {
            push_editor_candidate(
                &mut candidates,
                local_app_data,
                r"Programs\Microsoft VS Code\Code.exe",
            );
            push_program_files_candidates(
                &mut candidates,
                program_files,
                program_files_x86,
                r"Microsoft VS Code\Code.exe",
            );
            candidates.push(OsString::from("code"));
        }
        "cursor" => {
            push_editor_candidate(
                &mut candidates,
                local_app_data,
                r"Programs\Cursor\Cursor.exe",
            );
            push_program_files_candidates(
                &mut candidates,
                program_files,
                program_files_x86,
                r"Cursor\Cursor.exe",
            );
            candidates.push(OsString::from("cursor"));
        }
        "codium" | "vscodium" => {
            push_editor_candidate(
                &mut candidates,
                local_app_data,
                r"Programs\VSCodium\VSCodium.exe",
            );
            push_program_files_candidates(
                &mut candidates,
                program_files,
                program_files_x86,
                r"VSCodium\VSCodium.exe",
            );
            candidates.push(OsString::from("codium"));
        }
        "sublime" | "sublimetext" => {
            push_editor_candidate(
                &mut candidates,
                local_app_data,
                r"Programs\Sublime Text\sublime_text.exe",
            );
            push_program_files_candidates(
                &mut candidates,
                program_files,
                program_files_x86,
                r"Sublime Text\sublime_text.exe",
            );
            candidates.push(OsString::from("sublime_text"));
            candidates.push(OsString::from("subl"));
        }
        "notepadplusplus" => {
            push_program_files_candidates(
                &mut candidates,
                program_files,
                program_files_x86,
                r"Notepad++\notepad++.exe",
            );
            candidates.push(OsString::from("notepad++"));
        }
        "windsurf" => {
            push_editor_candidate(
                &mut candidates,
                local_app_data,
                r"Programs\Windsurf\Windsurf.exe",
            );
            push_program_files_candidates(
                &mut candidates,
                program_files,
                program_files_x86,
                r"Windsurf\Windsurf.exe",
            );
            candidates.push(OsString::from("windsurf"));
        }
        "zed" => {
            push_editor_candidate(&mut candidates, local_app_data, r"Programs\Zed\Zed.exe");
            push_program_files_candidates(
                &mut candidates,
                program_files,
                program_files_x86,
                r"Zed\Zed.exe",
            );
            candidates.push(OsString::from("zed"));
        }
        _ => {}
    }

    candidates
}

#[cfg(windows)]
fn windows_editor_candidates(editor: &str) -> Vec<OsString> {
    windows_editor_candidates_from_dirs(
        editor,
        std::env::var_os("LOCALAPPDATA").as_deref().map(Path::new),
        std::env::var_os("ProgramFiles").as_deref().map(Path::new),
        std::env::var_os("ProgramFiles(x86)")
            .as_deref()
            .map(Path::new),
    )
}

#[cfg(windows)]
#[derive(Debug, Clone, PartialEq, Eq)]
struct WindowsEditorProgram {
    program: OsString,
    command_processor_shim: bool,
}

#[cfg(windows)]
impl WindowsEditorProgram {
    fn executable(program: impl Into<OsString>) -> Self {
        Self {
            program: program.into(),
            command_processor_shim: false,
        }
    }

    fn command_shim(program: impl Into<OsString>) -> Self {
        Self {
            program: program.into(),
            command_processor_shim: true,
        }
    }
}

#[cfg(windows)]
fn windows_candidate_has_path_components(candidate: &OsString) -> bool {
    let path = Path::new(candidate);
    path.parent()
        .is_some_and(|parent| parent != Path::new(""))
        || path.file_name().is_none()
}

#[cfg(windows)]
fn windows_candidate_names(candidate: &OsString) -> Vec<OsString> {
    if Path::new(candidate).extension().is_some() {
        return vec![candidate.clone()];
    }

    let mut exe = candidate.clone();
    exe.push(".exe");
    let mut cmd = candidate.clone();
    cmd.push(".cmd");
    vec![exe, cmd]
}

#[cfg(windows)]
fn windows_editor_program_from_path(path: PathBuf) -> WindowsEditorProgram {
    if path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("cmd"))
    {
        WindowsEditorProgram::command_shim(path.into_os_string())
    } else {
        WindowsEditorProgram::executable(path.into_os_string())
    }
}

#[cfg(windows)]
fn resolve_windows_path_candidate_from_dirs(
    candidate: &OsString,
    path_dirs: &[PathBuf],
) -> Option<WindowsEditorProgram> {
    if windows_candidate_has_path_components(candidate) {
        return None;
    }

    let names = windows_candidate_names(candidate);

    for dir in path_dirs {
        for name in &names {
            let path = dir.join(name);
            if path.is_file() {
                return Some(windows_editor_program_from_path(path));
            }
        }
    }

    None
}

#[cfg(windows)]
fn resolve_windows_path_candidate(candidate: &OsString) -> Option<WindowsEditorProgram> {
    let path_dirs = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .unwrap_or_default();
    resolve_windows_path_candidate_from_dirs(candidate, &path_dirs)
}

#[cfg(windows)]
fn first_available_windows_candidate(candidates: &[OsString]) -> Option<WindowsEditorProgram> {
    candidates
        .iter()
        .find(|candidate| Path::new(candidate).is_file())
        .map(|candidate| WindowsEditorProgram::executable(candidate.clone()))
        .or_else(|| {
            candidates
                .iter()
                .filter(|candidate| !Path::new(candidate).is_absolute())
                .find_map(resolve_windows_path_candidate)
        })
}

fn build_editor_launch_spec(editor: &str, path: &Path) -> Option<EditorLaunchSpec> {
    let editor = trim_configured_editor(editor);
    if editor.is_empty() {
        return None;
    }
    if let Some(executable) = configured_executable(editor) {
        return Some(EditorLaunchSpec::new(executable.into_os_string(), path));
    }

    #[cfg(windows)]
    {
        let candidates = windows_editor_candidates(editor);
        if let Some(program) = first_available_windows_candidate(&candidates) {
            if program.command_processor_shim {
                return Some(EditorLaunchSpec::command_shim(program.program, path));
            }
            return Some(EditorLaunchSpec::new(program.program, path));
        }
        if !candidates.is_empty() {
            return None;
        }
    }

    #[cfg(target_os = "macos")]
    {
        return Some(EditorLaunchSpec {
            program: OsString::from("open"),
            args: vec![
                OsString::from("-a"),
                OsString::from(editor),
                path.as_os_str().to_os_string(),
            ],
        });
    }

    #[cfg(not(target_os = "macos"))]
    Some(EditorLaunchSpec::new(editor, path))
}

#[cfg(windows)]
fn windows_command_processor() -> OsString {
    std::env::var_os("SystemRoot")
        .map(|root| Path::new(&root).join(r"System32\cmd.exe"))
        .filter(|path| path.is_file())
        .map(PathBuf::into_os_string)
        .unwrap_or_else(|| OsString::from("cmd.exe"))
}

#[cfg(windows)]
fn spawn_windows_command_shim(spec: &EditorLaunchSpec) -> Result<(), String> {
    Command::new(windows_command_processor())
        .arg("/d")
        .arg("/c")
        .arg(&spec.program)
        .args(&spec.args)
        .spawn()
        .map(|_| ())
        .map_err(|err| format!("failed to open editor {:?}: {err}", spec.program))
}

fn spawn_editor(spec: EditorLaunchSpec) -> Result<(), String> {
    #[cfg(windows)]
    if spec.command_processor_shim {
        return spawn_windows_command_shim(&spec);
    }

    Command::new(&spec.program)
        .args(&spec.args)
        .spawn()
        .map(|_| ())
        .map_err(|err| format!("failed to open editor {:?}: {err}", spec.program))
}

fn open_path_with_default_viewer(path: &Path) -> Result<(), String> {
    tauri_plugin_opener::open_path(path.display().to_string(), None::<&str>)
        .map_err(|err| format!("failed to open path with default viewer: {err}"))
}

#[tauri::command]
async fn open_in_editor(path: String, editor: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let path = PathBuf::from(path);
        match build_editor_launch_spec(&editor, &path) {
            Some(spec) => spawn_editor(spec),
            None => open_path_with_default_viewer(&path),
        }
    })
    .await
    .map_err(|err| format!("editor launch task failed: {err}"))?
}

#[tauri::command]
async fn connect_daemon(app: AppHandle, state: State<'_, HitchClient>) -> Result<(), String> {
    let client = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || client.connect_and_handshake(&app))
        .await
        .map_err(|err| format!("daemon connection task failed: {err}"))?
}

/// Current Daemon Status + reason + log path for the in-window indicator
/// (ADR 0009). Pull complement to the pushed `hitch-status` event.
#[tauri::command]
fn get_daemon_status(state: State<'_, HitchClient>) -> StatusPayload {
    state.inner().status_payload()
}

/// Tail of the daemon log for the status popover's "View log" detail (ADR 0009).
#[tauri::command]
fn get_daemon_log_tail(state: State<'_, HitchClient>, lines: Option<usize>) -> Option<String> {
    read_log_tail(&state.inner().0.log_path, lines.unwrap_or(200))
}

/// Restart the daemon on demand (the popover/tray "Restart daemon" action).
/// Wraps the existing restart path so a wedged or failed daemon is recoverable
/// without the terminal (ADR 0009).
#[tauri::command]
async fn restart_daemon_command(
    app: AppHandle,
    state: State<'_, HitchClient>,
) -> Result<(), String> {
    let client = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        if let Ok(mut guard) = client.0.restart_guard.lock() {
            // A user-initiated restart is an explicit intent, not a crash loop —
            // clear the budget so it always proceeds.
            guard.reset();
        }
        client.restart_daemon_and_handshake(&app, "user requested daemon restart".to_string())?;
        Ok::<(), String>(())
    })
    .await
    .map_err(|err| format!("daemon restart task failed: {err}"))?
}

#[tauri::command]
async fn hitch_request(
    app: AppHandle,
    state: State<'_, HitchClient>,
    request: Request,
) -> Result<Response, String> {
    let client = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || client.send_request(&app, request))
        .await
        .map_err(|err| format!("daemon request task failed: {err}"))?
}

/// Fire-and-forget keystroke path (Slice 6, "input fast path"). The old
/// implementation ran a per-keystroke `spawn_blocking` that wrote the input frame
/// AND blocked a pool thread on the daemon's `Ack`; that made typing feel laggy
/// and, with concurrent tasks, could in principle reorder characters. We now just
/// enqueue the bytes on the ordered input lane and return immediately — the single
/// `hitch-input-writer` drain thread serialises the actual socket writes, so order
/// is guaranteed and no thread blocks per keystroke. Input is best-effort like
/// resize: a disconnected socket drops keystrokes inside the drain thread; the
/// only error we can surface here is the drain thread being gone, which never
/// happens in practice (it lives as long as the client). The frontend's
/// `void invoke("send_session_input", { sessionId, data })` ignores the result, so
/// returning `Response::Ack` immediately keeps the external contract intact.
#[tauri::command]
fn send_session_input(
    state: State<'_, HitchClient>,
    session_id: SessionId,
    data: String,
) -> Result<Response, String> {
    state
        .0
        .input_tx
        .send((session_id, data.into_bytes()))
        .map_err(|_| "input writer thread is gone".to_string())?;
    Ok(Response::Ack)
}

/// Register the webview's per-session output channel (ADR 0007). Any bytes that
/// arrived before registration are flushed immediately so a new session's first
/// prompt isn't dropped. Re-registration (e.g. the same session re-subscribing)
/// replaces the stored channel cleanly and drains whatever was staged since.
#[tauri::command]
fn register_session_output(
    state: State<'_, HitchClient>,
    session_id: SessionId,
    channel: Channel<InvokeResponseBody>,
) -> Result<(), String> {
    let mut router = state
        .0
        .output_router
        .lock()
        .map_err(|_| "output-router lock poisoned".to_string())?;
    router.register_channel(session_id, channel);
    Ok(())
}

/// Drop a session's output channel + any staged bytes (ADR 0007). Called when
/// the session closes or the webview tears its terminal down.
#[tauri::command]
fn unregister_session_output(
    state: State<'_, HitchClient>,
    session_id: SessionId,
) -> Result<(), String> {
    let mut router = state
        .0
        .output_router
        .lock()
        .map_err(|_| "output-router lock poisoned".to_string())?;
    router.unregister_channel(session_id);
    Ok(())
}

/// Report the maximize button's physical-pixel rectangle so the Windows window
/// subclass can hit-test it as the native caption max button (driving Snap
/// Layouts). A no-op off Windows; the frontend only calls it there.
#[tauri::command]
fn set_max_button_rect(left: i32, top: i32, right: i32, bottom: i32) {
    window_chrome::set_max_button_rect(left, top, right, bottom);
}

/// Menu-bar status line mirroring the four-state Daemon Status (ADR 0009). The
/// daemon keeps running after the window closes, so this is the honest signal
/// that Hitch has a background presence (ADR 0003). Always word + state, never
/// color alone (design principle #3).
fn tray_status_text(status: DaemonStatus, count: usize) -> String {
    match status {
        DaemonStatus::Starting => "Hitch — starting daemon…".to_string(),
        DaemonStatus::Unreachable => "Hitch — daemon unreachable".to_string(),
        DaemonStatus::Failed => "Hitch — daemon failed".to_string(),
        DaemonStatus::Running => match count {
            0 => "Hitch — running, no active sessions".to_string(),
            1 => "Hitch — running 1 session".to_string(),
            n => format!("Hitch — running {n} sessions"),
        },
    }
}

fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

fn handle_tray_menu_event(app: &AppHandle, event: MenuEvent) {
    match event.id.as_ref() {
        "show" => show_main_window(app),
        "view-log" => {
            // Open the daemon log in the user's default viewer (ADR 0009).
            let path = app.state::<HitchClient>().0.log_path.clone();
            let _ = tauri_plugin_opener::open_path(path.display().to_string(), None::<&str>);
        }
        "restart-daemon" => {
            // Restart off the main thread so the menu handler returns promptly.
            let app = app.clone();
            thread::spawn(move || {
                let client = app.state::<HitchClient>().inner().clone();
                if let Ok(mut guard) = client.0.restart_guard.lock() {
                    guard.reset();
                }
                if let Err(err) = client
                    .restart_daemon_and_handshake(&app, "user requested daemon restart".to_string())
                {
                    client.set_status(&app, DaemonStatus::Failed, Some(err));
                }
            });
        }
        "quit" => {
            // Full quit: stop the daemon (kills sessions) and exit the GUI. Run the
            // shutdown on a worker thread with a short bounded wait so a no-writer
            // fallback (which connects and waits for an Ack) can't hang the UI thread.
            let client = app.state::<HitchClient>().inner().clone();
            let (tx, rx) = mpsc::channel();
            thread::Builder::new()
                .name("hitch-daemon-quit-shutdown".into())
                .spawn(move || {
                    let _ = tx.send(client.request_daemon_shutdown());
                })
                .ok();
            let _ = rx.recv_timeout(Duration::from_secs(1));
            app.exit(0);
        }
        _ => {}
    }
}

fn tray_icon_as_template() -> bool {
    cfg!(target_os = "macos")
}

fn apply_platform_tray_behavior(builder: TrayIconBuilder<Wry>) -> TrayIconBuilder<Wry> {
    #[cfg(windows)]
    {
        // Windows users expect left click to restore the app and right click to
        // show the context menu. macOS keeps Tauri's default menu-bar behaviour.
        builder
            .show_menu_on_left_click(false)
            .on_tray_icon_event(|tray, event| {
                if let TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                } = event
                {
                    show_main_window(tray.app_handle());
                }
            })
    }
    #[cfg(not(windows))]
    {
        builder
    }
}

fn build_tray(app: &tauri::App) -> tauri::Result<()> {
    let status = MenuItem::with_id(
        app,
        "status",
        tray_status_text(DaemonStatus::Starting, 0),
        false,
        None::<&str>,
    )?;
    let show = MenuItem::with_id(app, "show", "Show Hitch", true, None::<&str>)?;
    let view_log = MenuItem::with_id(app, "view-log", "View daemon log", true, None::<&str>)?;
    let restart = MenuItem::with_id(app, "restart-daemon", "Restart daemon", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit Hitch", true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[
            &status,
            &PredefinedMenuItem::separator(app)?,
            &show,
            &view_log,
            &restart,
            &PredefinedMenuItem::separator(app)?,
            &quit,
        ],
    )?;

    let builder = TrayIconBuilder::with_id(TRAY_ID)
        .menu(&menu)
        .tooltip(tray_status_text(DaemonStatus::Starting, 0))
        .icon(tauri::include_image!("icons/tray.png"))
        .icon_as_template(tray_icon_as_template());
    let builder = apply_platform_tray_behavior(builder).on_menu_event(handle_tray_menu_event);
    builder.build(app)?;

    if let Ok(mut slot) = app.state::<HitchClient>().0.tray_status.lock() {
        *slot = Some(status);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        build_editor_launch_spec, describe_handshake_failure, encode_control_message,
        parse_spawned_daemon_pid, read_control_message, read_log_tail, recovery_mode_for_loss,
        run_probe, should_force_kill_daemon, tray_status_text, ControlMessage, CrashLoopGuard,
        DaemonStatus, ErrorCode, HitchClient, OutputRouter, ProbeError, ProtocolError, RecoveryMode,
        Request, Response, CRASH_LOOP_MAX, HEARTBEAT_LOST_REASON,
    };
    #[cfg(windows)]
    use super::{
        first_available_windows_candidate, resolve_windows_path_candidate_from_dirs,
        windows_editor_candidates_from_dirs, WindowsEditorProgram,
    };
    #[cfg(unix)]
    use super::{read_pty_payload, wait_for_socket_release};
    use hitch_core::SessionId;
    use hitch_proto::transport::{connect_daemon, DaemonListener};
    #[cfg(unix)]
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};
    use tauri::ipc::{Channel, InvokeResponseBody};

    fn recording_channel(received: Arc<Mutex<Vec<Vec<u8>>>>) -> Channel<InvokeResponseBody> {
        Channel::new(move |body| {
            match body {
                InvokeResponseBody::Raw(bytes) => received.lock().unwrap().push(bytes),
                InvokeResponseBody::Json(json) => panic!("unexpected JSON channel payload: {json}"),
            }
            Ok(())
        })
    }

    #[test]
    fn configured_editor_path_with_spaces_is_preserved_as_program() {
        let dir = std::env::temp_dir().join(format!(
            "hitch-editor-test-{}-{}",
            std::process::id(),
            "path-with-spaces"
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let editor = dir.join("Editor With Spaces.exe");
        std::fs::write(&editor, b"").unwrap();
        let worktree = std::path::Path::new(r"C:\repo with spaces");

        let configured = format!(" \"{}\" ", editor.display());
        let spec = build_editor_launch_spec(&configured, worktree).unwrap();

        assert_eq!(spec.program, editor.as_os_str());
        assert_eq!(spec.args, vec![worktree.as_os_str().to_os_string()]);

        std::fs::remove_file(editor).unwrap();
        std::fs::remove_dir(dir).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn windows_display_names_resolve_to_installer_locations_and_path_fallbacks() {
        let local = std::path::Path::new(r"C:\Users\Ada\AppData\Local");
        let program_files = std::path::Path::new(r"C:\Program Files");
        let program_files_x86 = std::path::Path::new(r"C:\Program Files (x86)");

        let code = windows_editor_candidates_from_dirs(
            "Visual Studio Code",
            Some(local),
            Some(program_files),
            Some(program_files_x86),
        );
        assert!(code.contains(
            &local
                .join(r"Programs\Microsoft VS Code\Code.exe")
                .into_os_string()
        ));
        assert!(code.contains(
            &program_files
                .join(r"Microsoft VS Code\Code.exe")
                .into_os_string()
        ));
        assert!(code.contains(&std::ffi::OsString::from("code")));

        let cursor =
            windows_editor_candidates_from_dirs("Cursor", Some(local), Some(program_files), None);
        assert!(cursor.contains(&local.join(r"Programs\Cursor\Cursor.exe").into_os_string()));
        assert!(cursor.contains(&std::ffi::OsString::from("cursor")));

        let notepad = windows_editor_candidates_from_dirs(
            "Notepad++",
            Some(local),
            Some(program_files),
            Some(program_files_x86),
        );
        assert!(notepad.contains(
            &program_files_x86
                .join(r"Notepad++\notepad++.exe")
                .into_os_string()
        ));
        assert!(notepad.contains(&std::ffi::OsString::from("notepad++")));
    }

    #[cfg(windows)]
    #[test]
    fn windows_candidate_selection_prefers_existing_executable_before_path_fallback() {
        let dir = std::env::temp_dir().join(format!(
            "hitch-editor-test-{}-{}",
            std::process::id(),
            "candidate-precedence"
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let editor = dir.join("Code.exe");
        std::fs::write(&editor, b"").unwrap();
        let candidates = vec![
            editor.as_os_str().to_os_string(),
            std::ffi::OsString::from("code"),
        ];

        assert_eq!(
            first_available_windows_candidate(&candidates),
            Some(WindowsEditorProgram::executable(
                editor.as_os_str().to_os_string()
            ))
        );

        std::fs::remove_file(editor).unwrap();
        std::fs::remove_dir(dir).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn windows_path_fallback_resolves_cmd_shim_before_launch() {
        let dir = std::env::temp_dir().join(format!(
            "hitch-editor-test-{}-{}",
            std::process::id(),
            "path-cmd-shim"
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let shim = dir.join("code.cmd");
        std::fs::write(&shim, b"").unwrap();

        assert_eq!(
            resolve_windows_path_candidate_from_dirs(
                &std::ffi::OsString::from("code"),
                &[dir.clone()],
            ),
            Some(WindowsEditorProgram::command_shim(
                shim.as_os_str().to_os_string()
            ))
        );

        std::fs::remove_file(shim).unwrap();
        std::fs::remove_dir(dir).unwrap();
    }

    #[test]
    fn session_opened_preserves_output_that_raced_ahead_of_initial_event() {
        let session_id = SessionId::new();
        let received = Arc::new(Mutex::new(Vec::new()));
        let mut router = OutputRouter::default();

        router.send_or_stage(session_id, b"early-prompt".to_vec());
        router.prepare_fresh_registration(session_id);

        assert_eq!(
            router.staging.get(&session_id).cloned(),
            Some(b"early-prompt".to_vec())
        );

        router.register_channel(session_id, recording_channel(received.clone()));
        assert_eq!(
            received.lock().unwrap().clone(),
            vec![b"early-prompt".to_vec()]
        );
        assert!(!router.staging.contains_key(&session_id));
    }

    #[test]
    fn session_opened_stages_replay_until_fresh_channel_registered() {
        let session_id = SessionId::new();
        let old_received = Arc::new(Mutex::new(Vec::new()));
        let new_received = Arc::new(Mutex::new(Vec::new()));
        let mut router = OutputRouter::default();

        router.prepare_fresh_registration(session_id);
        router.register_channel(session_id, recording_channel(old_received.clone()));
        router.send_or_stage(session_id, b"before-reconnect".to_vec());
        assert_eq!(
            old_received.lock().unwrap().clone(),
            vec![b"before-reconnect".to_vec()]
        );

        router.prepare_fresh_registration(session_id);
        router.send_or_stage(session_id, b"replayed-scrollback".to_vec());

        assert_eq!(
            old_received.lock().unwrap().clone(),
            vec![b"before-reconnect".to_vec()],
            "replay must not be delivered over the stale pre-reconnect channel"
        );
        assert_eq!(
            router.staging.get(&session_id).cloned(),
            Some(b"replayed-scrollback".to_vec())
        );

        router.register_channel(session_id, recording_channel(new_received.clone()));
        assert_eq!(
            new_received.lock().unwrap().clone(),
            vec![b"replayed-scrollback".to_vec()]
        );
        assert!(!router.staging.contains_key(&session_id));

        router.send_or_stage(session_id, b"live-after-registration".to_vec());
        assert_eq!(
            new_received.lock().unwrap().clone(),
            vec![
                b"replayed-scrollback".to_vec(),
                b"live-after-registration".to_vec(),
            ]
        );
    }

    #[test]
    fn reconnect_session_opened_discards_stale_staging_even_without_channel() {
        let session_id = SessionId::new();
        let mut router = OutputRouter::default();

        router.prepare_fresh_registration(session_id);
        router.send_or_stage(session_id, b"missed-before-reconnect".to_vec());
        assert!(router.staging.contains_key(&session_id));

        router.prepare_fresh_registration(session_id);
        assert!(!router.staging.contains_key(&session_id));
    }

    #[test]
    fn registration_flush_failure_restages_staged_bytes() {
        let session_id = SessionId::new();
        let received = Arc::new(Mutex::new(Vec::new()));
        let mut router = OutputRouter::default();

        router.send_or_stage(session_id, b"early-prompt".to_vec());

        router.register_channel(
            session_id,
            Channel::new(|_| Err(std::io::Error::other("closed channel").into())),
        );

        assert!(!router.channels.contains_key(&session_id));
        assert_eq!(
            router.staging.get(&session_id).cloned(),
            Some(b"early-prompt".to_vec())
        );

        router.register_channel(session_id, recording_channel(received.clone()));
        assert_eq!(
            received.lock().unwrap().clone(),
            vec![b"early-prompt".to_vec()]
        );
        assert!(!router.staging.contains_key(&session_id));
    }

    #[test]
    fn send_failure_removes_dead_channel_and_stages_payload() {
        let session_id = SessionId::new();
        let received = Arc::new(Mutex::new(Vec::new()));
        let mut router = OutputRouter::default();

        router.prepare_fresh_registration(session_id);
        router.register_channel(
            session_id,
            Channel::new(|_| Err(std::io::Error::other("closed channel").into())),
        );

        router.send_or_stage(session_id, b"not-lost".to_vec());

        assert!(!router.channels.contains_key(&session_id));
        assert_eq!(
            router.staging.get(&session_id).cloned(),
            Some(b"not-lost".to_vec())
        );

        router.register_channel(session_id, recording_channel(received.clone()));
        assert_eq!(received.lock().unwrap().clone(), vec![b"not-lost".to_vec()]);
        assert!(!router.staging.contains_key(&session_id));
    }

    #[test]
    fn session_closed_forgets_opened_session_state() {
        let session_id = SessionId::new();
        let mut router = OutputRouter::default();

        router.prepare_fresh_registration(session_id);
        router.close_session(session_id);
        router.send_or_stage(session_id, b"new-early-prompt".to_vec());
        router.prepare_fresh_registration(session_id);

        assert_eq!(
            router.staging.get(&session_id).cloned(),
            Some(b"new-early-prompt".to_vec())
        );
    }

    #[test]
    fn tray_status_text_reflects_status_and_count() {
        assert_eq!(
            tray_status_text(DaemonStatus::Starting, 2),
            "Hitch — starting daemon…"
        );
        assert_eq!(
            tray_status_text(DaemonStatus::Unreachable, 0),
            "Hitch — daemon unreachable"
        );
        assert_eq!(
            tray_status_text(DaemonStatus::Failed, 0),
            "Hitch — daemon failed"
        );
        assert_eq!(
            tray_status_text(DaemonStatus::Running, 0),
            "Hitch — running, no active sessions"
        );
        assert_eq!(
            tray_status_text(DaemonStatus::Running, 1),
            "Hitch — running 1 session"
        );
        assert_eq!(
            tray_status_text(DaemonStatus::Running, 4),
            "Hitch — running 4 sessions"
        );
    }

    #[test]
    fn hook_binary_path_tracks_bundled_daemon_sibling() {
        use std::time::{SystemTime, UNIX_EPOCH};

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("hitch-sidecar-path-{nonce}"));
        std::fs::create_dir_all(&dir).unwrap();
        let daemon = dir.join("hitch-daemon");
        let hook = dir.join("hitch-hook");
        std::fs::write(&daemon, "").unwrap();

        assert_eq!(super::hook_binary_path_for_daemon(&daemon), None);

        std::fs::write(&hook, "").unwrap();
        assert_eq!(super::hook_binary_path_for_daemon(&daemon), Some(hook));

        let suffixed_daemon = dir.join(format!(
            "hitch-daemon-x86_64-pc-windows-msvc{}",
            std::env::consts::EXE_SUFFIX
        ));
        let suffixed_hook = dir.join(format!(
            "hitch-hook-x86_64-pc-windows-msvc{}",
            std::env::consts::EXE_SUFFIX
        ));
        std::fs::write(&suffixed_daemon, "").unwrap();
        assert_eq!(super::hook_binary_path_for_daemon(&suffixed_daemon), None);
        std::fs::write(&suffixed_hook, "").unwrap();
        assert_eq!(
            super::hook_binary_path_for_daemon(&suffixed_daemon),
            Some(suffixed_hook)
        );

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn read_log_tail_returns_last_n_lines_and_handles_missing_file() {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("hitch-tauri-logtail-{nonce}.log"));

        // Missing file → None, so callers fall back to a generic reason.
        assert_eq!(read_log_tail(&path, 5), None);

        std::fs::write(&path, "l1\nl2\nl3\nl4\n").unwrap();
        assert_eq!(read_log_tail(&path, 2).as_deref(), Some("l3\nl4"));
        // Asking for more lines than exist returns them all.
        assert_eq!(read_log_tail(&path, 10).as_deref(), Some("l1\nl2\nl3\nl4"));
        // The single-line reason path picks the most recent line.
        assert_eq!(read_log_tail(&path, 1).as_deref(), Some("l4"));

        // An empty (whitespace-only) log reads as None, not an empty reason.
        std::fs::write(&path, "\n\n").unwrap();
        assert_eq!(read_log_tail(&path, 3), None);

        let _ = std::fs::remove_file(&path);
    }

    #[cfg(unix)]
    #[test]
    fn read_daemon_pidfile_skips_stale_pid_but_returns_locked_one() {
        use std::os::unix::io::AsRawFd;
        use std::time::{SystemTime, UNIX_EPOCH};
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let socket_path = std::env::temp_dir().join(format!("hitch-pidfile-{nonce}.sock"));
        let pidfile = hitch_proto::transport::pidfile_path(&socket_path);
        std::fs::write(&pidfile, "424242").unwrap();

        // No live owner holds the lock → the pidfile is stale → no kill target.
        assert_eq!(super::read_daemon_pidfile(&socket_path), None);

        // Hold the advisory lock like a live daemon would → the pid is returned.
        let held = std::fs::File::open(&pidfile).unwrap();
        // SAFETY: flock on a valid fd held for the duration of the assertion.
        assert_eq!(
            unsafe { libc::flock(held.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
            0
        );
        assert_eq!(super::read_daemon_pidfile(&socket_path), Some(424242));
        drop(held);

        // Lock released → stale again.
        assert_eq!(super::read_daemon_pidfile(&socket_path), None);
        let _ = std::fs::remove_file(&pidfile);
    }

    #[cfg(unix)]
    #[test]
    fn force_kill_pid_selection_prefers_cached_pid_when_pidfile_is_missing_or_stale() {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let socket_path = std::env::temp_dir().join(format!("hitch-force-pid-{nonce}.sock"));
        let pidfile = hitch_proto::transport::pidfile_path(&socket_path);

        assert_eq!(
            super::daemon_pid_for_force_kill(&socket_path, Some(12345)),
            Some(12345)
        );
        assert_eq!(super::daemon_pid_for_force_kill(&socket_path, None), None);

        std::fs::write(&pidfile, "424242").unwrap();
        assert_eq!(
            super::daemon_pid_for_force_kill(&socket_path, Some(12345)),
            Some(12345)
        );
        assert_eq!(super::daemon_pid_for_force_kill(&socket_path, None), None);
        let _ = std::fs::remove_file(&pidfile);
    }

    #[cfg(windows)]
    #[test]
    fn windows_force_kill_uses_cached_daemon_pid() {
        let incompatible = "restarting incompatible daemon: unsupported protocol";
        assert_eq!(
            super::windows_force_kill_pid_for_reason(incompatible, Some(424242)),
            Some(424242)
        );
        assert_eq!(
            super::windows_force_kill_pid_for_reason(HEARTBEAT_LOST_REASON, Some(424242)),
            Some(424242)
        );
        assert_eq!(
            super::windows_force_kill_pid_for_reason("daemon socket closed", Some(424242)),
            None
        );
        assert_eq!(
            super::windows_force_kill_pid_for_reason(incompatible, None),
            None
        );
    }

    #[test]
    fn crash_loop_guard_stops_after_max_attempts_in_window() {
        let window = Duration::from_secs(60);
        let mut guard = CrashLoopGuard::new(CRASH_LOOP_MAX, window);
        let t0 = Instant::now();

        // The first CRASH_LOOP_MAX attempts within the window are allowed.
        for i in 0..CRASH_LOOP_MAX {
            assert!(
                guard.allow(t0 + Duration::from_secs(i as u64)),
                "attempt {i} within budget should be allowed"
            );
        }
        // The next attempt within the window trips the guard.
        assert!(
            !guard.allow(t0 + Duration::from_secs(CRASH_LOOP_MAX as u64)),
            "exceeding the cap within the window must stop respawning"
        );

        // Once the window has fully elapsed, old attempts roll off and a fresh
        // attempt is allowed again.
        assert!(guard.allow(t0 + window + Duration::from_secs(1)));

        // An explicit reset (a user-initiated restart) clears the budget.
        let mut guard = CrashLoopGuard::new(2, window);
        assert!(guard.allow(t0));
        assert!(guard.allow(t0));
        assert!(!guard.allow(t0));
        guard.reset();
        assert!(guard.allow(t0));
    }

    #[test]
    fn healthy_handshake_reset_breaks_startup_failure_sequence() {
        let window = Duration::from_secs(60);
        let mut guard = CrashLoopGuard::new(CRASH_LOOP_MAX, window);
        let t0 = Instant::now();

        for i in 0..CRASH_LOOP_MAX {
            assert!(guard.allow(t0 + Duration::from_secs(i as u64)));
        }

        guard.reset();

        for i in 0..CRASH_LOOP_MAX {
            assert!(
                guard.allow(t0 + Duration::from_secs((10 + i) as u64)),
                "attempt {i} after a healthy handshake should start a fresh budget"
            );
        }
    }

    #[test]
    fn heartbeat_and_handshake_timeouts_use_restart_recovery() {
        assert_eq!(
            recovery_mode_for_loss(HEARTBEAT_LOST_REASON),
            RecoveryMode::RestartDaemon
        );
        assert_eq!(
            recovery_mode_for_loss(
                "daemon handshake failed: timed out waiting for daemon response"
            ),
            RecoveryMode::RestartDaemon
        );
        assert!(should_force_kill_daemon(HEARTBEAT_LOST_REASON));
        assert!(should_force_kill_daemon(
            "daemon handshake failed: timed out waiting for daemon response"
        ));
        assert_eq!(
            recovery_mode_for_loss("daemon socket closed"),
            RecoveryMode::Reconnect
        );
        assert!(!should_force_kill_daemon("daemon socket closed"));
        // An incompatible daemon can't be trusted to shut itself down, so the
        // restart path force-kills it rather than waiting on a graceful unbind.
        assert!(should_force_kill_daemon(
            "restarting incompatible daemon: client protocol 13 != daemon protocol 12"
        ));
    }

    #[test]
    fn handshake_failure_message_covers_every_non_hello_outcome() {
        // A protocol-rejection Error surfaces the daemon's own message.
        assert_eq!(
            describe_handshake_failure(&Ok(Response::Error {
                error: ProtocolError::new(ErrorCode::UnsupportedProtocol, "bad version"),
            })),
            "bad version"
        );
        // A transport/parse error (the case that used to leak past recovery)
        // still yields a reason, so it takes the restart path like the rest.
        assert_eq!(
            describe_handshake_failure(&Err("connection reset".to_string())),
            "connection reset"
        );
        // An unexpected variant is described rather than silently dropped.
        assert!(
            describe_handshake_failure(&Ok(Response::Ack)).starts_with("unexpected hello response")
        );
    }

    #[test]
    fn shutdown_over_new_connection_waits_for_daemon_ack() {
        use std::io::{BufReader, Write};
        use std::time::{SystemTime, UNIX_EPOCH};

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "hitch-shutdown-new-connection-{}-{nonce}.sock",
            std::process::id()
        ));
        let listener = hitch_proto::transport::DaemonListener::bind(&path).unwrap();

        let server = std::thread::spawn(move || {
            let stream = listener.accept().unwrap();
            let mut reader = BufReader::new(stream);
            let message = read_control_message(&mut reader).unwrap().unwrap();
            assert_eq!(
                message,
                ControlMessage::request(99, Request::ShutdownDaemon)
            );
            let mut stream = reader.into_inner();
            let ack = encode_control_message(&ControlMessage::response(99, Response::Ack)).unwrap();
            stream.write_all(&ack).unwrap();
            stream.flush().unwrap();
            std::thread::sleep(Duration::from_millis(50));
        });

        HitchClient::request_daemon_shutdown_over_new_connection(&path, 99).unwrap();
        server.join().unwrap();
        #[cfg(unix)]
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn run_probe_returns_worker_value_within_timeout() {
        let got = run_probe("hitch-test-probe-fast", Duration::from_secs(5), || 7).ok();
        assert_eq!(got, Some(7));
    }

    #[test]
    fn run_probe_reports_timeout_when_worker_outlasts_deadline() {
        // Worker blocks well past the caller's deadline.
        let res = run_probe("hitch-test-probe-timeout", Duration::from_millis(50), || {
            std::thread::sleep(Duration::from_millis(400));
            1u8
        });
        assert!(matches!(res, Err(ProbeError::Timeout)));
    }

    #[test]
    fn run_probe_bounds_leaked_workers_to_one_per_name() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        // A worker that signals when it actually starts running, so the test can
        // tell whether a *second* thread was spawned while the first is blocked.
        static STARTS: AtomicUsize = AtomicUsize::new(0);
        STARTS.store(0, Ordering::SeqCst);

        let release = Arc::new(Mutex::new(()));
        // Hold the lock so the first worker blocks inside `work`.
        let held = release.lock().unwrap();

        let name = "hitch-test-probe-leak-bound";
        let release_1 = Arc::clone(&release);
        // First probe: worker starts, increments STARTS, then blocks on the
        // mutex the test is holding. Caller times out and parks the worker.
        let first = run_probe(name, Duration::from_millis(80), move || {
            STARTS.fetch_add(1, Ordering::SeqCst);
            let _block = release_1.lock().unwrap();
            0u8
        });
        assert!(matches!(first, Err(ProbeError::Timeout)));
        assert_eq!(STARTS.load(Ordering::SeqCst), 1, "first worker should run");

        // Second probe with the same name while the first is still blocked: it
        // must NOT spawn another worker (STARTS stays 1) and must report timeout
        // by waiting on the parked worker.
        let release_2 = Arc::clone(&release);
        let second = run_probe(name, Duration::from_millis(80), move || {
            STARTS.fetch_add(1, Ordering::SeqCst);
            let _block = release_2.lock().unwrap();
            0u8
        });
        assert!(matches!(second, Err(ProbeError::Timeout)));
        assert_eq!(
            STARTS.load(Ordering::SeqCst),
            1,
            "second same-name probe must not spawn a second worker while the first is blocked",
        );

        // Release the blocked worker and let it drain so we don't leak across
        // other tests sharing the process.
        drop(held);
        // Drain the parked worker: a fresh probe of the same name now waits for
        // the previous one to finish, then runs its own work.
        let third = run_probe(name, Duration::from_secs(5), || 9u8);
        assert_eq!(third.ok(), Some(9));
    }

    #[test]
    fn spawned_daemon_pid_is_parsed_from_detach_stdout() {
        assert_eq!(parse_spawned_daemon_pid(b"12345\n"), Some(12345));
        assert_eq!(
            parse_spawned_daemon_pid(b"Finished dev profile\n9876\r\n"),
            Some(9876)
        );
        assert_eq!(parse_spawned_daemon_pid(b""), None);
    }

    #[cfg(unix)]
    #[test]
    fn restart_refuses_to_spawn_over_a_live_socket() {
        use std::time::{SystemTime, UNIX_EPOCH};

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("hitch-tauri-socket-{nonce}"));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("daemon.sock");

        let listener = UnixListener::bind(&path).unwrap();
        let err = wait_for_socket_release(&path, Duration::from_millis(150)).unwrap_err();
        assert!(err.contains("still accepting connections"));

        drop(listener);
        assert!(wait_for_socket_release(&path, Duration::from_millis(150)).is_ok());

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn final_handshake_failure_clears_attached_connection_state() {
        use std::time::{SystemTime, UNIX_EPOCH};

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "hitch-final-handshake-failure-{}-{nonce}.sock",
            std::process::id()
        ));
        let listener = DaemonListener::bind(&path).unwrap();
        let server = std::thread::spawn(move || listener.accept().unwrap());
        let writer = connect_daemon(&path).unwrap();
        let server_stream = server.join().unwrap();

        let client = HitchClient::new();
        client
            .0
            .connected
            .store(true, std::sync::atomic::Ordering::SeqCst);
        *client.0.writer.lock().unwrap() = Some(writer);
        assert!(client.is_connected());

        client.clear_connection_state(
            "daemon hello failed after restart: unsupported protocol",
            true,
        );

        assert!(
            !client.is_connected(),
            "failed final Hello must not leave the client treating a stale writer as connected"
        );
        assert!(
            client.0.writer.lock().unwrap().is_none(),
            "failed final Hello must drop the attached writer so the next call reconnects"
        );

        drop(server_stream);
        #[cfg(unix)]
        let _ = std::fs::remove_file(path);
    }

    #[cfg(unix)]
    #[test]
    fn restart_recovery_preserves_writer_for_shutdown_request() {
        let client = HitchClient::new();
        let (writer, reader) = UnixStream::pair().unwrap();
        reader
            .set_read_timeout(Some(Duration::from_millis(200)))
            .unwrap();

        client
            .0
            .connected
            .store(true, std::sync::atomic::Ordering::SeqCst);
        *client.0.writer.lock().unwrap() = Some(writer);

        client.clear_connection_state(HEARTBEAT_LOST_REASON, false);

        assert!(!client.is_connected());
        assert!(client.0.writer.lock().unwrap().is_some());

        client.request_daemon_shutdown();

        let mut reader = std::io::BufReader::new(reader);
        let message = read_control_message(&mut reader).unwrap().unwrap();
        assert_eq!(message, ControlMessage::request(1, Request::ShutdownDaemon));
    }

    #[cfg(unix)]
    #[test]
    fn oversized_input_frame_does_not_announce_missing_payload() {
        let client = HitchClient::new();
        let (writer, reader) = UnixStream::pair().unwrap();
        reader
            .set_read_timeout(Some(Duration::from_millis(200)))
            .unwrap();

        client
            .0
            .connected
            .store(true, std::sync::atomic::Ordering::SeqCst);
        *client.0.writer.lock().unwrap() = Some(writer);

        let oversized = vec![0_u8; hitch_proto::MAX_PTY_FRAME_LEN + 1];
        client.write_input_frame(SessionId::new(), &oversized);

        let mut reader = std::io::BufReader::new(reader);
        let err = read_control_message(&mut reader).unwrap_err();
        assert!(
            matches!(
                err.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
            ),
            "oversized payload must not write a SendSessionInput control frame first: {err:?}"
        );
    }

    /// Slice 6: the fire-and-forget input lane must preserve keystroke ORDER. A
    /// burst of keystrokes pushed onto `input_tx` should arrive on the socket as
    /// `SendSessionInput` control+payload pairs in exactly the order enqueued,
    /// proving the single drain thread serialises writes (no reordering, no lost
    /// frames). We read back through a `UnixStream` pair so this exercises the
    /// real frame encoding the daemon would see, minus a live daemon.
    #[cfg(unix)]
    #[test]
    fn input_lane_preserves_keystroke_order() {
        let client = HitchClient::new();
        let (writer, reader) = UnixStream::pair().unwrap();
        reader
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();

        // Attach the writer end exactly the way `attach_stream` would, so the
        // drain thread finds a live socket to write to.
        client
            .0
            .connected
            .store(true, std::sync::atomic::Ordering::SeqCst);
        *client.0.writer.lock().unwrap() = Some(writer);

        // A rapid, distinct sequence so any reordering is detectable. One byte per
        // keystroke mirrors typing.
        let session_id = SessionId::new();
        let keys: Vec<u8> = (b'a'..=b'z').collect();
        for &key in &keys {
            client.0.input_tx.send((session_id, vec![key])).unwrap();
        }

        // Each enqueued keystroke is one control message (SendSessionInput) plus
        // one PTY payload frame. Read them back in order and collect the payload
        // bytes; they must match the order we sent.
        let mut reader = std::io::BufReader::new(reader);
        let mut received = Vec::with_capacity(keys.len());
        for _ in 0..keys.len() {
            let message = read_control_message(&mut reader).unwrap().unwrap();
            match message {
                ControlMessage::Request {
                    request:
                        Request::SendSessionInput {
                            session_id: got_session,
                            byte_count,
                        },
                    ..
                } => {
                    assert_eq!(got_session, session_id);
                    assert_eq!(byte_count, 1);
                }
                other => panic!("expected SendSessionInput, got {other:?}"),
            }
            let payload = read_pty_payload(&mut reader).unwrap();
            assert_eq!(payload.len(), 1);
            received.push(payload[0]);
        }

        assert_eq!(received, keys, "keystrokes arrived out of order");
    }
}

/// Make held keys repeat in the terminal on macOS.
///
/// WKWebView honours the `ApplePressAndHoldEnabled` user default. When it is on
/// (the OS default), holding a key shows the accent-character popup and
/// SUPPRESSES key repeat — so holding `j` in vim does nothing. We register the
/// default to `NO`, process-scoped (registration domain is never persisted to
/// disk), before the webview reads it. This is surprising but intentional and
/// app-wide: the trade-off is that holding a key in any input field no longer
/// opens the accent popup, which we accept for a terminal-native app.
#[cfg(target_os = "macos")]
fn disable_press_and_hold() {
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2_foundation::{NSDictionary, NSNumber, NSString, NSUserDefaults};

    let key = NSString::from_str("ApplePressAndHoldEnabled");
    let value: Retained<AnyObject> = NSNumber::numberWithBool(false).into();
    let defaults = NSDictionary::from_retained_objects(&[&*key], &[value]);
    // SAFETY: `defaults` is an `NSDictionary<NSString, AnyObject>`, matching the
    // type `registerDefaults:` expects.
    unsafe {
        NSUserDefaults::standardUserDefaults().registerDefaults(&defaults);
    }
}

#[cfg(feature = "packaged-smoke")]
fn packaged_smoke_enabled() -> bool {
    matches!(env::var("HITCH_PACKAGED_SMOKE_TEST").as_deref(), Ok("1"))
}

#[cfg(feature = "packaged-smoke")]
fn smoke_temp_project_root() -> PathBuf {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    env::temp_dir().join(format!(
        "hitch-packaged-smoke-{}-{timestamp}",
        std::process::id()
    ))
}

#[cfg(feature = "packaged-smoke")]
fn response_error(context: &str, response: Response) -> String {
    match response {
        Response::Error { error } => format!("{context}: {}", error.message),
        other => format!("{context}: unexpected response {other:?}"),
    }
}

#[cfg(feature = "packaged-smoke")]
fn smoke_request(
    client: &HitchClient,
    app: &AppHandle,
    request: Request,
    context: &str,
) -> Result<Response, String> {
    client
        .send_request(app, request)
        .map_err(|err| format!("{context}: {err}"))
}

#[cfg(feature = "packaged-smoke")]
fn run_packaged_smoke(app: AppHandle, client: HitchClient) -> Result<(), String> {
    let project_root = smoke_temp_project_root();
    let mut project_id = None;
    let mut session_id = None;

    let result = (|| {
        fs::create_dir_all(&project_root).map_err(|err| {
            format!(
                "failed to create smoke project {}: {err}",
                project_root.display()
            )
        })?;

        client.connect_and_handshake(&app).map_err(|err| {
            format!("failed to connect and handshake with packaged daemon: {err}")
        })?;

        let project = match smoke_request(
            &client,
            &app,
            Request::AddProject {
                root: project_root.clone(),
            },
            "failed to add smoke project",
        )? {
            Response::Projects { mut projects } if projects.len() == 1 => projects.remove(0),
            other => return Err(response_error("failed to add smoke project", other)),
        };
        project_id = Some(project.id);

        let parent = SessionParent::Project(project.id);
        let session = match smoke_request(
            &client,
            &app,
            Request::OpenSession {
                parent,
                name: "packaged-smoke-shell".into(),
                command: None,
                cols: 80,
                rows: 24,
            },
            "failed to open smoke shell session",
        )? {
            Response::SessionOpened { session, .. } => session,
            other => return Err(response_error("failed to open smoke shell session", other)),
        };
        session_id = Some(session.id);

        match smoke_request(
            &client,
            &app,
            Request::ListSessions {
                parent: Some(parent),
            },
            "failed to list smoke sessions",
        )? {
            Response::Sessions { sessions }
                if sessions.iter().any(|listed| listed.id == session.id) => {}
            Response::Sessions { .. } => {
                return Err("smoke shell session was not returned by ListSessions".to_string());
            }
            other => return Err(response_error("failed to list smoke sessions", other)),
        }

        match smoke_request(
            &client,
            &app,
            Request::CloseSession {
                session_id: session.id,
                kill_process: true,
            },
            "failed to close smoke shell session",
        )? {
            Response::Ack => session_id = None,
            other => return Err(response_error("failed to close smoke shell session", other)),
        }

        match smoke_request(
            &client,
            &app,
            Request::RemoveProject {
                project_id: project.id,
                force: true,
            },
            "failed to remove smoke project",
        )? {
            Response::Ack => project_id = None,
            other => return Err(response_error("failed to remove smoke project", other)),
        }

        match smoke_request(
            &client,
            &app,
            Request::ShutdownDaemon,
            "failed to shut down smoke daemon",
        )? {
            Response::Ack => Ok(()),
            other => Err(response_error("failed to shut down smoke daemon", other)),
        }
    })();

    if client.is_connected() {
        if let Some(id) = session_id {
            let _ = client.send_request(
                &app,
                Request::CloseSession {
                    session_id: id,
                    kill_process: true,
                },
            );
        }
        if let Some(id) = project_id {
            let _ = client.send_request(
                &app,
                Request::RemoveProject {
                    project_id: id,
                    force: true,
                },
            );
        }
        if result.is_err() {
            client.request_daemon_shutdown();
        }
    }
    let _ = fs::remove_dir_all(&project_root);

    result
}

fn start_daemon_connection_on_launch(app: &tauri::App) {
    let app_handle = app.handle().clone();
    let client = app.state::<HitchClient>().inner().clone();
    thread::Builder::new()
        .name("hitch-daemon-launch-connect".into())
        .spawn(move || {
            if let Err(err) = client.connect_and_handshake(&app_handle) {
                client.set_status(&app_handle, DaemonStatus::Failed, Some(err));
            }
        })
        .expect("failed to spawn daemon launch connection thread");
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(HitchClient::new())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            #[cfg(target_os = "macos")]
            disable_press_and_hold();
            // Debug builds run their own isolated daemon (`.hitch-dev`); label the
            // window so it's obvious which build you're looking at when a dev build
            // and an installed release build are open side by side.
            #[cfg(debug_assertions)]
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.set_title("Hitch (dev)");
            }
            // Frameless Windows window draws its own caption controls; subclass
            // the window proc so the maximize button still drives Snap Layouts.
            // No-op on macOS (native Overlay title bar) and Linux.
            if let Some(window) = app.get_webview_window("main") {
                window_chrome::install(&window);
            }
            build_tray(app)?;
            start_daemon_connection_on_launch(app);
            Ok(())
        })
        .on_window_event(|window, event| {
            // Closing the window hides it instead of quitting: the daemon and the
            // menu-bar presence stay alive until the user picks "Quit Hitch".
            if let WindowEvent::CloseRequested { api, .. } = event {
                let _ = window.hide();
                api.prevent_close();
            }
        })
        .invoke_handler(tauri::generate_handler![
            open_in_editor,
            connect_daemon,
            hitch_request,
            send_session_input,
            register_session_output,
            unregister_session_output,
            get_daemon_status,
            get_daemon_log_tail,
            restart_daemon_command,
            set_max_button_rect
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(move |_app_handle, _event| {
            #[cfg(feature = "packaged-smoke")]
            if matches!(_event, RunEvent::Ready) && packaged_smoke_enabled() {
                let app_handle = _app_handle.clone();
                let client = app_handle.state::<HitchClient>().inner().clone();
                thread::Builder::new()
                    .name("hitch-packaged-smoke".into())
                    .spawn(move || {
                        let exit_code = match run_packaged_smoke(app_handle.clone(), client) {
                            Ok(()) => 0,
                            Err(err) => {
                                eprintln!("packaged smoke test failed: {err}");
                                1
                            }
                        };
                        std::process::exit(exit_code);
                    })
                    .expect("failed to spawn packaged smoke thread");
            }
        });
}
