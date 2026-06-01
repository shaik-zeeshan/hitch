//! `hitch-desktop` (src-tauri) — the thin Tauri client (ADR 0005).
//!
//! The GUI process holds no git/pty/store/agent logic: it only keeps a daemon
//! socket connection, starts the daemon when needed, and relays `hitch-proto`
//! requests/responses/events to Tauri IPC.

use std::collections::{HashMap, HashSet};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use hitch_core::SessionId;
use hitch_proto::{
    encode_control_message, encode_pty_frame, ControlMessage, ErrorCode, Event, ProtocolError,
    Request, RequestId, Response, PROTOCOL_VERSION,
};
use serde::Serialize;
use tauri::ipc::{Channel, InvokeResponseBody};
use tauri::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Manager, State, WindowEvent, Wry};

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
    // A wedged daemon (missed heartbeat) and an incompatible one are both
    // untrustworthy to shut themselves down on request — an incompatible daemon
    // may not even parse our ShutdownDaemon message. SIGKILL by pid instead of
    // asking nicely and waiting on a graceful unbind that may never come.
    reason == HEARTBEAT_LOST_REASON || reason.starts_with(INCOMPATIBLE_DAEMON_PREFIX)
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
fn read_daemon_pidfile(socket_path: &Path) -> Option<u32> {
    let path = hitch_proto::transport::pidfile_path(socket_path);
    let pid: u32 = std::fs::read_to_string(&path).ok()?.trim().parse().ok()?;
    if pidfile_is_stale(&path) {
        return None;
    }
    Some(pid)
}

fn daemon_pid_for_force_kill(socket_path: &Path, cached_pid: Option<u32>) -> Option<u32> {
    cached_pid.or_else(|| read_daemon_pidfile(socket_path))
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

#[cfg(not(unix))]
fn pidfile_is_stale(_path: &Path) -> bool {
    false
}

fn wait_for_socket_release(path: &Path, timeout: Duration) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    loop {
        if UnixStream::connect(path).is_err() {
            return Ok(());
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
///
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
    writer: Mutex<Option<UnixStream>>,
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
    /// Pid reported by the last successful Hello handshake. Auto-recovery uses
    /// it to SIGKILL a heartbeat-wedged daemon before waiting on socket release.
    daemon_pid: Mutex<Option<u32>>,
    /// Daemon log path, computed once from `$HOME` to match the daemon writer.
    log_path: PathBuf,
}

/// The tray's stable id, used to look it up for tooltip updates.
const TRAY_ID: &str = "hitch-tray";

impl HitchClient {
    fn new() -> Self {
        Self(Arc::new(HitchClientInner {
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
            log_path: daemon_log_path(),
        }))
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

    fn cached_daemon_pid(&self) -> Option<u32> {
        self.0.daemon_pid.lock().ok().and_then(|slot| *slot)
    }

    fn force_kill_daemon(&self, preferred_pid: Option<u32>) -> Result<(), String> {
        // Heartbeat recovery has a trustworthy pid from the last successful Hello,
        // but `restart_daemon` must disconnect before it can respawn; pass that pid
        // in before `mark_disconnected` clears it. Protocol-mismatch restarts pass
        // `None` and use the pidfile instead, because a cached pid from an older
        // daemon could be stale while an upgraded daemon owns the socket.
        let daemon_pid = daemon_pid_for_force_kill(&self.0.socket_path, preferred_pid)
            .ok_or_else(|| "cannot force-restart daemon: daemon pid is unknown".to_string())?;
        #[cfg(unix)]
        unsafe {
            if libc::kill(daemon_pid as i32, libc::SIGKILL) != 0 {
                let err = io::Error::last_os_error();
                if err.raw_os_error() != Some(libc::ESRCH) {
                    return Err(format!("failed to kill wedged daemon {daemon_pid}: {err}"));
                }
            }
        }
        #[cfg(not(unix))]
        {
            return Err("force-restarting the daemon is unsupported on this platform".into());
        }
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

        let stream = match UnixStream::connect(&self.0.socket_path) {
            Ok(stream) => stream,
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
    ///
    /// The key invariant: *any* non-`Hello` outcome — a protocol-rejection
    /// `Error`, an unexpected variant, a response this client can't even
    /// deserialize, or a transport error — is treated as "the running daemon is
    /// incompatible or wedged" and triggers a force-restart. A response we cannot
    /// parse is itself proof of incompatibility, so it must take the restart path
    /// rather than leak to the caller (the previous `Ok(Response::Error)`-only
    /// branch let parse and transport errors surface as a bare mismatch).
    fn connect_and_handshake(&self, app: &AppHandle) -> Result<(), String> {
        let hello = Request::Hello {
            client_name: "hitch-desktop".into(),
            protocol_version: PROTOCOL_VERSION,
        };
        let mut last_err = String::new();
        for attempt in 0..MAX_HANDSHAKE_ATTEMPTS {
            self.connect(app)?;
            match self.send_request(app, hello.clone()) {
                Ok(Response::Hello { daemon_pid, .. }) => {
                    self.set_daemon_pid(Some(daemon_pid));
                    self.mark_running(app);
                    return Ok(());
                }
                other => {
                    last_err = describe_handshake_failure(&other);
                    // Don't burn the final attempt on a restart we won't retry.
                    if attempt + 1 == MAX_HANDSHAKE_ATTEMPTS {
                        break;
                    }
                    self.restart_daemon(app, format!("{INCOMPATIBLE_DAEMON_PREFIX}: {last_err}"))?;
                }
            }
        }
        if self.is_connected() {
            self.handle_connection_lost(app, &format!("daemon handshake failed: {last_err}"));
        }
        Err(last_err)
    }

    fn handshake_after_restart(&self, app: &AppHandle) -> Result<(), String> {
        match self.send_request(
            app,
            Request::Hello {
                client_name: "hitch-desktop".into(),
                protocol_version: PROTOCOL_VERSION,
            },
        ) {
            Ok(Response::Hello { daemon_pid, .. }) => {
                self.set_daemon_pid(Some(daemon_pid));
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

    fn attach_stream(&self, app: &AppHandle, stream: UnixStream) -> Result<(), String> {
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
        self.start_reader(app.clone(), stream, generation);
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
            let force_kill_pid = if reason == HEARTBEAT_LOST_REASON {
                self.cached_daemon_pid()
            } else {
                None
            };
            self.mark_disconnected(app, reason.clone());
            self.record_spawn_attempt(app)?;
            self.set_status(app, DaemonStatus::Starting, None);

            // For a wedged or incompatible daemon, SIGKILL by pid is the fast path
            // — but it's best-effort: a daemon predating the pidfile (the very
            // stale-daemon-across-an-upgrade case) reports no pid, so we must not
            // hard-fail on it. In every case we then wait for the socket to be
            // released so we never spawn a second daemon over a live one.
            if force_kill {
                let _ = self.force_kill_daemon(force_kill_pid);
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
                Err("timed out waiting for daemon response".into())
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

    fn start_reader(&self, app: AppHandle, stream: UnixStream, generation: u64) {
        let client = self.clone();
        thread::Builder::new()
            .name("hitch-daemon-reader".into())
            .spawn(move || {
                let result = reader_loop(&app, &client, stream);
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
        // The cached pid is only meaningful while we're handshook with that exact
        // daemon. Once disconnected it's stale — a daemon swapped underneath us
        // (upgrade, crash+respawn) leaves an incompatible daemon at the socket
        // whose pid lives only in the pidfile. Clearing here means `force_kill_daemon`
        // falls through to the pidfile for the protocol-mismatch case instead of
        // SIGKILLing a stale/reused pid while the real daemon keeps the socket.
        if let Ok(mut slot) = self.0.daemon_pid.lock() {
            *slot = None;
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

    /// Fire-and-forget a `ShutdownDaemon` request (full quit). We do not wait for
    /// the Ack: the caller is about to exit the GUI process, and restart recovery
    /// may still have a usable writer even after marking the connection unavailable.
    fn request_daemon_shutdown(&self) {
        let request_id = self.0.next_request_id.fetch_add(1, Ordering::SeqCst);
        let Ok(bytes) = encode_control_message(&ControlMessage::request(
            request_id,
            Request::ShutdownDaemon,
        )) else {
            return;
        };
        if let Ok(mut guard) = self.0.writer.lock() {
            if let Some(writer) = guard.as_mut() {
                let _ = writer.write_all(&bytes);
                let _ = writer.flush();
            }
        }
    }

    fn spawn_daemon(&self) -> Result<(), String> {
        // In debug builds, always use `cargo run` so the daemon is compiled from
        // current source. Relying on the sibling binary in target/debug risks using
        // a stale build with a different protocol version when only one crate was
        // rebuilt (e.g. `cargo tauri dev` recompiles hitch-desktop but not
        // hitch-daemon).
        #[cfg(not(debug_assertions))]
        if let Some(path) = daemon_binary_path() {
            let hook_path = hook_binary_path_for_daemon(&path).ok_or_else(|| {
                format!(
                    "hitch-hook binary was not found next to {}; bundled agent hooks cannot be installed",
                    path.display()
                )
            })?;
            let output = Command::new(&path)
                .arg("--socket")
                .arg(&self.0.socket_path)
                .arg("--hook-helper")
                .arg(&hook_path)
                .arg("--detach")
                .output()
                .map_err(|err| format!("failed to spawn {}: {err}", path.display()))?;
            if output.status.success() {
                return Ok(());
            }
            return Err(format!(
                "hitch-daemon failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }

        #[cfg(debug_assertions)]
        {
            let hook_output = Command::new("cargo")
                .arg("build")
                .arg("-p")
                .arg("hitch-hook")
                .output()
                .map_err(|err| format!("failed to run `cargo build -p hitch-hook`: {err}"))?;
            if !hook_output.status.success() {
                return Err(format!(
                    "`cargo build -p hitch-hook` failed: {}{}",
                    String::from_utf8_lossy(&hook_output.stdout),
                    String::from_utf8_lossy(&hook_output.stderr)
                ));
            }

            let output = Command::new("cargo")
                .arg("run")
                .arg("-p")
                .arg("hitch-daemon")
                .arg("--")
                .arg("--socket")
                .arg(&self.0.socket_path)
                .arg("--detach")
                .output()
                .map_err(|err| format!("failed to run `cargo run -p hitch-daemon`: {err}"))?;
            if output.status.success() {
                return Ok(());
            }
            return Err(format!(
                "`cargo run -p hitch-daemon` failed: {}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ));
        }

        #[cfg(not(debug_assertions))]
        Err("hitch-daemon binary was not found next to the app; set HITCH_DAEMON_PATH".into())
    }

    fn wait_for_daemon(&self) -> Result<UnixStream, String> {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut last_error = None;
        while Instant::now() < deadline {
            match UnixStream::connect(&self.0.socket_path) {
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

fn reader_loop(app: &AppHandle, client: &HitchClient, stream: UnixStream) -> io::Result<()> {
    let mut reader = BufReader::new(stream);
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
    let sibling = dir.join("hitch-daemon");
    if sibling.is_file() {
        return Some(sibling);
    }

    None
}

#[cfg(any(not(debug_assertions), test))]
fn hook_binary_path_for_daemon(daemon: &Path) -> Option<PathBuf> {
    let dir = daemon.parent()?;
    let hook = dir.join("hitch-hook");
    hook.is_file().then_some(hook)
}

#[derive(Debug, Clone, Serialize)]
struct DisconnectedPayload {
    reason: String,
}

/// Path to the daemon's log. MUST match the daemon's own `daemon_log_path`
/// (same `$HOME`-based per-instance dir, `.hitch/daemon.log` for release and
/// `.hitch-dev/daemon.log` for debug) so the tail the GUI reads is the file the
/// daemon writes — never derived from the socket parent, to avoid drift. The
/// GUI and the daemon agree on the namespace because both honour their own
/// build profile (ADR 0009).
fn daemon_log_path() -> PathBuf {
    home_dir()
        .join(hitch_proto::transport::instance_dir_name())
        .join("daemon.log")
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
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

#[tauri::command]
async fn send_session_input(
    app: AppHandle,
    state: State<'_, HitchClient>,
    session_id: SessionId,
    data: String,
) -> Result<Response, String> {
    let client = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let bytes = data.into_bytes();
        client.send_request_with_payload(
            &app,
            Request::SendSessionInput {
                session_id,
                byte_count: bytes.len() as u32,
            },
            Some(bytes),
        )
    })
    .await
    .map_err(|err| format!("session input task failed: {err}"))?
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
            // Full quit: stop the daemon (kills sessions) and exit the GUI.
            app.state::<HitchClient>().request_daemon_shutdown();
            app.exit(0);
        }
        _ => {}
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
        .icon_as_template(true)
        .on_menu_event(handle_tray_menu_event);
    builder.build(app)?;

    if let Ok(mut slot) = app.state::<HitchClient>().0.tray_status.lock() {
        *slot = Some(status);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        describe_handshake_failure, read_control_message, read_log_tail, recovery_mode_for_loss,
        should_force_kill_daemon, tray_status_text, wait_for_socket_release, ControlMessage,
        CrashLoopGuard, DaemonStatus, ErrorCode, HitchClient, OutputRouter, ProtocolError,
        RecoveryMode, Request, Response, CRASH_LOOP_MAX, HEARTBEAT_LOST_REASON,
    };
    use hitch_core::SessionId;
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
        assert!(!should_force_kill_daemon(
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
            build_tray(app)?;
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
            connect_daemon,
            hitch_request,
            send_session_input,
            register_session_output,
            unregister_session_output,
            get_daemon_status,
            get_daemon_log_tail,
            restart_daemon_command
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
