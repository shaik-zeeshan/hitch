//! Multi-daemon connection pool for remote Daemons reached over SSH (issue #27,
//! ADR 0014).
//!
//! The desktop already owns ONE local daemon connection (`HitchClient` in
//! `lib.rs`). This module adds N remote connections, one per saved **SSH Host**,
//! keyed by `DaemonScopeId` (`ssh:<target>`). Each remote connection:
//!
//! 1. spawns `ssh -o BatchMode=yes -- <target> hitch daemon proxy` (the same
//!    command the Test Connection path uses, see `ssh.rs`),
//! 2. performs the Hitch protocol Hello/version handshake over the child's stdio
//!    (NOT a socket — the remote proxy bridges to the remote daemon's local
//!    endpoint),
//! 3. runs the same client loop as the local path — request dispatch, an event
//!    pump that re-emits scope-tagged events to the webview, and a Ping/Pong
//!    heartbeat — but over the child's stdin/stdout rather than a `DaemonStream`.
//!
//! ## Stream abstraction
//!
//! `hitch-proto`'s framing helpers (`encode_control_message`, `encode_pty_frame`)
//! and `read_control_message`/`read_pty_payload` in `lib.rs` are generic over
//! `Write`/`BufRead`/`Read`. So rather than retrofit `DaemonStream` to carry a
//! child's split stdio, the remote path uses the child's `ChildStdin` as the
//! writer and a `BufReader<ChildStdout>` as the reader directly. The exact same
//! frames flow; only the underlying transport differs.
//!
//! ## Event tagging (relied on by issue #28+)
//!
//! Remote daemon events are emitted to the webview as `hitch-scope-event` with
//! payload `{ scope: "ssh:<target>", event: <Event> }`. Per-scope Daemon Status
//! is emitted as `hitch-scope-status` with `{ scope, status, reason }`. The local
//! path keeps emitting the untagged `hitch-event` / `hitch-status` it always did,
//! so the local flow is byte-for-byte unchanged.
//!
//! ## Backoff
//!
//! A dropped/failed remote connection auto-reconnects with exponential backoff +
//! jitter per ADR 0014: ~2s, 5s, 15s, 30s, then capped ~60s. `retry_ssh_host`
//! resets the backoff and reconnects immediately.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;

#[cfg(windows)]
use std::os::windows::process::CommandExt;
#[cfg(windows)]
use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

use hitch_core::SessionId;
use hitch_proto::{
    encode_control_message, encode_pty_frame, ControlMessage, Event, OsFamily, Request, RequestId,
    Response, PROTOCOL_VERSION, UPLOAD_CHUNK_BYTES,
};
use serde::Serialize;
use tauri::ipc::{Channel, InvokeResponseBody};
use tauri::{AppHandle, Emitter};

use crate::ssh::{self, HandshakeOutcome};
use crate::{read_control_message, read_pty_payload, OutputRouter};

/// Scope-tagged event payload emitted to the webview (`hitch-scope-event`).
#[derive(Debug, Clone, Serialize)]
struct ScopeEventPayload<'a> {
    scope: &'a str,
    event: &'a Event,
}

/// Per-scope Daemon Status payload emitted to the webview (`hitch-scope-status`).
#[derive(Debug, Clone, Serialize)]
struct ScopeStatusPayload<'a> {
    scope: &'a str,
    status: RemoteStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
}

/// The four-state Daemon Status for a remote scope, mirroring the frontend's
/// `DaemonStatus`. Serialized kebab-case to match.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum RemoteStatus {
    Starting,
    Running,
    Unreachable,
    Failed,
}

/// Handshake/response deadline for a remote scope, mirroring the local heartbeat
/// tolerances (a Ping shares the single SSH stdio stream with other control
/// requests, so the timeout stays generous).
const REMOTE_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(15);
const REMOTE_HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(20);
const REMOTE_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
const REMOTE_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(20);

/// Exponential backoff schedule for remote reconnect (ADR 0014): ~2s, 5s, 15s,
/// 30s, then capped ~60s. Pure so it is unit-testable; jitter is applied on top
/// by [`backoff_delay`].
pub fn backoff_base_secs(attempt: u32) -> u64 {
    match attempt {
        0 => 2,
        1 => 5,
        2 => 15,
        3 => 30,
        _ => 60,
    }
}

/// Backoff delay for a reconnect attempt, with +/- up to ~20% jitter so a fleet
/// of hosts dropped together (VPN blip) doesn't reconnect in lockstep. Bounded
/// below at 1s. `rand_unit` is the jitter source in [0,1), injected for tests.
pub fn backoff_delay(attempt: u32, rand_unit: f64) -> Duration {
    let base = backoff_base_secs(attempt) as f64;
    // Jitter factor in [0.8, 1.2).
    let factor = 0.8 + 0.4 * rand_unit.clamp(0.0, 1.0);
    let secs = (base * factor).max(1.0);
    Duration::from_secs_f64(secs)
}

/// A cheap process-local jitter source in [0,1) without pulling in `rand`. Uses
/// the nanosecond clock — adequate for de-correlating reconnect storms.
fn jitter_unit() -> f64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    (nanos % 1_000) as f64 / 1_000.0
}

// ---------------------------------------------------------------------------
// Approach C: candidate orchestration (pure decision logic, ADR 0014 amendment)
// ---------------------------------------------------------------------------

/// One remote-command candidate for invoking `hitch daemon proxy` on the host.
/// Each variant maps to the remote argv (after `ssh … -- <target>`) used to
/// launch the daemon proxy. The first connect tries these in [`candidate_sequence`]
/// order; a learned `exe_path` short-circuits to a direct launch on reconnect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProxyCandidate {
    /// The known Unix self-install location. `~/.local/bin/hitch` is passed as a
    /// LITERAL argv element (no local shell expansion); the remote login shell
    /// (`$SHELL -c`) expands the leading `~`.
    KnownLocation,
    /// Bare `hitch`, resolved by the remote login PATH. Covers manual installs and
    /// the Windows registry PATH set by the installer.
    BarePath,
    /// A learned absolute daemon path from a prior Hello (`exe_path`), launched
    /// directly on reconnect — shell-free and PATH-free.
    CachedExePath(String),
}

impl ProxyCandidate {
    /// The remote argv for this candidate, i.e. the tokens appended after
    /// `ssh … -- <target>`. The first token is the program; the rest are
    /// `daemon proxy`. `~` in [`ProxyCandidate::KnownLocation`] is intentionally
    /// NOT expanded here — it is a literal argv element the remote shell expands.
    pub fn remote_args(&self) -> Vec<String> {
        let program = match self {
            ProxyCandidate::KnownLocation => "~/.local/bin/hitch".to_string(),
            ProxyCandidate::BarePath => "hitch".to_string(),
            ProxyCandidate::CachedExePath(path) => path.clone(),
        };
        vec![program, "daemon".to_string(), "proxy".to_string()]
    }
}

/// The first-connect candidate sequence (no cached `exe_path`): try the known
/// self-install location, then bare `hitch`. Pure so its order/contents are
/// unit-tested without live SSH.
pub fn candidate_sequence() -> Vec<ProxyCandidate> {
    vec![ProxyCandidate::KnownLocation, ProxyCandidate::BarePath]
}

/// Decide whether a failed candidate should fall through to the next candidate
/// or surface immediately.
///
/// The candidate sequence exists to LOCATE a working `hitch` binary, so a failure
/// that is specific to the program we invoked should fall through and try the next
/// location:
/// - `MissingHitch` — the binary genuinely wasn't at that location.
/// - `ProxyStartup` — ssh connected and ran the command, but no/garbage Hello came
///   back. This is the bucket a Windows remote lands in when the known-location
///   candidate (`~/.local/bin/hitch`) isn't a runnable program there: the remote
///   shell's "not found"/"not recognized" wording (cmd.exe, PowerShell) doesn't
///   match the POSIX `MissingHitch` patterns, so we must NOT depend on classifying
///   every shell's message — we fall through and let the bare-`hitch` (PATH/registry)
///   candidate prove itself. Bare `hitch` is exactly what Test Connection runs, so
///   if that succeeds the attach now succeeds too.
///
/// Host-level failures are identical for EVERY candidate (same host), so retrying
/// them on the next candidate just doubles latency — they surface immediately:
/// `Auth`/`HostKey`/`Network`, the terminal `ProtocolMismatch` (a real hitch that's
/// the wrong version — report it, don't mask it), and `None` (no classified
/// category, e.g. a local ssh spawn error).
pub fn should_fall_through(category: Option<ssh::FailureCategory>) -> bool {
    matches!(
        category,
        Some(ssh::FailureCategory::MissingHitch | ssh::FailureCategory::ProxyStartup)
    )
}

/// Decide whether a cached `exe_path` that just failed should trigger
/// re-discovery (clear the cache + re-run the candidate sequence). The remote
/// moved/updated/removed the binary iff the failure classifies as `MissingHitch`;
/// any other category is a transient/auth/network problem the normal backoff
/// handles, so the cache stays.
pub fn should_rediscover_cached_path(category: Option<ssh::FailureCategory>) -> bool {
    matches!(category, Some(ssh::FailureCategory::MissingHitch))
}

/// The writer half of a remote connection: the SSH child's stdin. Frames are
/// written to it exactly as they would be to a `DaemonStream`.
type RemoteWriter = ChildStdin;

/// One remote daemon connection's mutable state. Cloned `Arc` handles are shared
/// between the reader thread, heartbeat thread, and request dispatch.
struct RemoteConnection {
    target: String,
    scope_id: String,
    next_request_id: AtomicU64,
    connected: AtomicBool,
    /// Bumped on each (re)connect so a stale reader/heartbeat from a superseded
    /// connection can't tear down a newer one.
    generation: AtomicU64,
    writer: Mutex<Option<RemoteWriter>>,
    pending: Mutex<HashMap<RequestId, mpsc::Sender<Response>>>,
    /// Per-session PTY output routing for THIS scope (session ids are unique only
    /// per daemon, so each scope owns its own router).
    output_router: Mutex<OutputRouter>,
    /// The SSH child process handle, so a disconnect/removal can kill it.
    child: Mutex<Option<Child>>,
    /// Reconnect attempt counter, for backoff. Reset on a successful connect and
    /// by `retry_ssh_host`.
    attempt: AtomicU64,
    /// The remote daemon's OS family, captured from its Hello (issue #31). Drives
    /// remote-platform path quoting when inserting uploaded paths. Defaults to Unix
    /// until the first successful handshake fills it in.
    os_family: Mutex<OsFamily>,
    /// The remote daemon's own executable path, learned from its Hello `exe_path`
    /// (approach C, ADR 0014 amendment). Cached IN MEMORY only — re-learned each
    /// session, never persisted to the localStorage host entry. When present, a
    /// reconnect launches this exact binary directly (shell-free, PATH-free); a
    /// `MissingHitch` from it (the remote moved/updated the binary) clears the
    /// cache and re-runs the candidate probe.
    exe_path: Mutex<Option<String>>,
    /// Set while a removal/disconnect is intentional, so the reader thread's EOF
    /// does not trigger auto-reconnect.
    shutting_down: AtomicBool,
    /// Set while a reconnect loop is in flight, so concurrent triggers collapse.
    reconnecting: AtomicBool,
}

impl RemoteConnection {
    fn new(target: String, scope_id: String) -> Self {
        Self {
            target,
            scope_id,
            next_request_id: AtomicU64::new(1),
            connected: AtomicBool::new(false),
            generation: AtomicU64::new(0),
            writer: Mutex::new(None),
            pending: Mutex::new(HashMap::new()),
            output_router: Mutex::new(OutputRouter::default()),
            child: Mutex::new(None),
            attempt: AtomicU64::new(0),
            os_family: Mutex::new(OsFamily::Unix),
            exe_path: Mutex::new(None),
            shutting_down: AtomicBool::new(false),
            reconnecting: AtomicBool::new(false),
        }
    }

    fn is_connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
            && self
                .writer
                .lock()
                .map(|w| w.is_some())
                .unwrap_or(false)
    }
}

/// The remote connection pool: a managed Tauri state mapping each SSH Host scope
/// id to its connection. The local daemon connection is owned separately by
/// `HitchClient`; this pool never touches it.
#[derive(Clone)]
pub struct SshConnections(Arc<SshConnectionsInner>);

struct SshConnectionsInner {
    connections: Mutex<HashMap<String, Arc<RemoteConnection>>>,
    /// Cancel flags for in-flight upload batches, keyed by batch id (issue #31).
    /// `cancel_upload` flips a batch's flag; the streaming task checks it before
    /// each file/chunk and aborts the in-flight upload so nothing is inserted.
    upload_cancels: Mutex<HashMap<String, Arc<AtomicBool>>>,
}

/// One file's result in an upload batch reported back to the frontend. `state`
/// distinguishes a real upload from a per-file rejection (a dropped directory),
/// so the frontend can toast the rejection copy and still insert the uploads.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum UploadFileResult {
    /// The file uploaded; `remote_path` is the actual remote path to insert.
    Uploaded { name: String, remote_path: String },
    /// The path was a directory; recursive upload isn't supported (ADR 0014).
    RejectedDirectory { name: String },
    /// The path could not be read/stat'd locally; carries the error for a toast.
    Failed { name: String, error: String },
}

/// The outcome of an upload batch returned to the frontend (issue #31). `os_family`
/// tells the frontend which shell quoting to use for the inserted remote paths;
/// `cancelled` is true when the user cancelled before completion (nothing should
/// be inserted then).
#[derive(Debug, Clone, Serialize)]
pub struct UploadBatchResult {
    #[serde(rename = "osFamily")]
    pub os_family: RemoteOsFamily,
    pub cancelled: bool,
    pub files: Vec<UploadFileResult>,
}

/// Serializable mirror of [`OsFamily`] for the upload batch result (the proto enum
/// isn't `Serialize` for Tauri's IPC the way we want the casing).
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RemoteOsFamily {
    Unix,
    Windows,
}

impl From<OsFamily> for RemoteOsFamily {
    fn from(value: OsFamily) -> Self {
        match value {
            OsFamily::Unix => RemoteOsFamily::Unix,
            OsFamily::Windows => RemoteOsFamily::Windows,
        }
    }
}

/// Per-file progress emitted to the webview as `hitch-upload-progress` while a
/// batch streams (issue #31). The frontend updates a loading toast from it.
#[derive(Debug, Clone, Serialize)]
struct UploadProgress<'a> {
    #[serde(rename = "batchId")]
    batch_id: &'a str,
    #[serde(rename = "fileIndex")]
    file_index: usize,
    #[serde(rename = "fileCount")]
    file_count: usize,
    #[serde(rename = "fileName")]
    file_name: &'a str,
    #[serde(rename = "sentBytes")]
    sent_bytes: u64,
    #[serde(rename = "totalBytes")]
    total_bytes: u64,
}

impl SshConnections {
    pub fn new() -> Self {
        Self(Arc::new(SshConnectionsInner {
            connections: Mutex::new(HashMap::new()),
            upload_cancels: Mutex::new(HashMap::new()),
        }))
    }

    fn connection(&self, scope_id: &str) -> Option<Arc<RemoteConnection>> {
        self.0
            .connections
            .lock()
            .ok()
            .and_then(|map| map.get(scope_id).cloned())
    }

    /// Reconcile the pool to exactly the given SSH Host targets. New hosts get a
    /// connection (and an initial connect attempt); removed hosts are
    /// disconnected and dropped. Idempotent: an unchanged host keeps its live
    /// connection. Called on app launch and on every host add/remove.
    pub fn set_hosts(&self, app: &AppHandle, targets: Vec<String>) {
        let wanted: HashMap<String, String> = targets
            .into_iter()
            .filter_map(|target| {
                ssh::normalize_target(&target)
                    .ok()
                    .map(|t| (format!("ssh:{t}"), t))
            })
            .collect();

        // Disconnect + drop any scope no longer wanted.
        let to_remove: Vec<String> = {
            let map = match self.0.connections.lock() {
                Ok(map) => map,
                Err(_) => return,
            };
            map.keys()
                .filter(|id| !wanted.contains_key(*id))
                .cloned()
                .collect()
        };
        for scope_id in to_remove {
            self.disconnect_scope(&scope_id);
        }

        // Add + connect any newly-wanted scope.
        for (scope_id, target) in wanted {
            let already = self
                .0
                .connections
                .lock()
                .map(|map| map.contains_key(&scope_id))
                .unwrap_or(false);
            if already {
                continue;
            }
            let connection = Arc::new(RemoteConnection::new(target, scope_id.clone()));
            if let Ok(mut map) = self.0.connections.lock() {
                map.insert(scope_id.clone(), connection.clone());
            }
            self.spawn_connect(app, connection, true);
        }
    }

    /// Retry a host now: reset its backoff and reconnect immediately. The Retry
    /// Now affordance on an unreachable/failed host row (ADR 0014) calls this.
    pub fn retry(&self, app: &AppHandle, target: &str) {
        let Ok(normalized) = ssh::normalize_target(target) else {
            return;
        };
        let scope_id = format!("ssh:{normalized}");
        let Some(connection) = self.connection(&scope_id) else {
            return;
        };
        connection.attempt.store(0, Ordering::SeqCst);
        connection.shutting_down.store(false, Ordering::SeqCst);
        // Tear any existing connection down so the reconnect starts clean.
        kill_child(&connection);
        self.spawn_connect(app, connection, true);
    }

    /// Disconnect and forget a scope (host removal, ADR 0014). Kills the SSH
    /// child (which drops the proxy and its bridge); the remote daemon keeps
    /// running. Drops the connection from the pool.
    pub fn disconnect_scope(&self, scope_id: &str) {
        if let Some(connection) = self.connection(scope_id) {
            connection.shutting_down.store(true, Ordering::SeqCst);
            connection.connected.store(false, Ordering::SeqCst);
            connection.generation.fetch_add(1, Ordering::SeqCst);
            if let Ok(mut writer) = connection.writer.lock() {
                *writer = None;
            }
            kill_child(&connection);
        }
        if let Ok(mut map) = self.0.connections.lock() {
            map.remove(scope_id);
        }
    }

    /// Dispatch a request to a remote scope and block for its response. Mirrors
    /// the local `dispatch_request` but over the SSH child's stdin.
    pub fn send_request(
        &self,
        scope_id: &str,
        request: Request,
        pty_payload: Option<Vec<u8>>,
    ) -> Result<Response, String> {
        let connection = self
            .connection(scope_id)
            .ok_or_else(|| format!("no remote daemon connection for scope {scope_id}"))?;
        dispatch_remote_request(&connection, request, pty_payload, REMOTE_REQUEST_TIMEOUT)
    }

    /// Enqueue fire-and-forget session input to a remote scope.
    pub fn send_session_input(&self, scope_id: &str, session_id: SessionId, bytes: Vec<u8>) {
        let Some(connection) = self.connection(scope_id) else {
            return;
        };
        write_remote_input(&connection, session_id, &bytes);
    }

    /// Stream a batch of local files to a remote Session (issue #31, ADR 0014).
    /// Runs synchronously on the caller's blocking task: each file is `begin`/
    /// `chunk*`/`finish`'d through the same per-scope request path as every other
    /// command, so each chunk is one request/response turn and interactive PTY
    /// traffic interleaves naturally between chunks. Directories are rejected per
    /// file (recursive upload isn't supported yet). Progress is emitted to the
    /// webview as `hitch-upload-progress`; `cancel_upload(batch_id)` aborts the
    /// in-flight file and stops the batch so nothing is inserted. Returns the
    /// per-file results plus the remote OS family for path quoting.
    pub fn upload_files(
        &self,
        app: &AppHandle,
        scope_id: &str,
        batch_id: &str,
        session_id: SessionId,
        paths: Vec<String>,
    ) -> Result<UploadBatchResult, String> {
        let connection = self
            .connection(scope_id)
            .ok_or_else(|| format!("no remote daemon connection for scope {scope_id}"))?;
        let os_family = connection
            .os_family
            .lock()
            .map(|f| *f)
            .unwrap_or(OsFamily::Unix);

        // Register a cancel flag for the batch so `cancel_upload` can stop it.
        let cancel = Arc::new(AtomicBool::new(false));
        if let Ok(mut map) = self.0.upload_cancels.lock() {
            map.insert(batch_id.to_string(), cancel.clone());
        }

        let mut files = Vec::with_capacity(paths.len());
        let file_count = paths.len();
        let mut cancelled = false;
        for (file_index, path) in paths.iter().enumerate() {
            if cancel.load(Ordering::SeqCst) {
                cancelled = true;
                break;
            }
            match upload_one_file(
                app,
                &connection,
                batch_id,
                session_id,
                Path::new(path),
                file_index,
                file_count,
                &cancel,
            ) {
                Ok(Some(result)) => files.push(result),
                // `None` = the in-flight file was cancelled; stop the batch.
                Ok(None) => {
                    cancelled = true;
                    break;
                }
                Err(result) => files.push(result),
            }
        }

        if let Ok(mut map) = self.0.upload_cancels.lock() {
            map.remove(batch_id);
        }
        Ok(UploadBatchResult {
            os_family: os_family.into(),
            cancelled,
            files,
        })
    }

    /// Cancel an in-flight upload batch (issue #31). Flips its cancel flag so the
    /// streaming task aborts the current file and stops; nothing is inserted.
    pub fn cancel_upload(&self, batch_id: &str) {
        if let Ok(map) = self.0.upload_cancels.lock() {
            if let Some(flag) = map.get(batch_id) {
                flag.store(true, Ordering::SeqCst);
            }
        }
    }

    /// Register a webview output channel for a session on a remote scope.
    pub fn register_session_output(
        &self,
        scope_id: &str,
        session_id: SessionId,
        channel: Channel<InvokeResponseBody>,
    ) -> Result<(), String> {
        let connection = self
            .connection(scope_id)
            .ok_or_else(|| format!("no remote daemon connection for scope {scope_id}"))?;
        connection
            .output_router
            .lock()
            .map_err(|_| "remote output-router lock poisoned".to_string())?
            .register_channel(session_id, channel);
        Ok(())
    }

    /// Unregister a webview output channel for a session on a remote scope.
    pub fn unregister_session_output(
        &self,
        scope_id: &str,
        session_id: SessionId,
    ) -> Result<(), String> {
        if let Some(connection) = self.connection(scope_id) {
            if let Ok(mut router) = connection.output_router.lock() {
                router.unregister_channel(session_id);
            }
        }
        Ok(())
    }

    /// Spawn the connect-and-handshake on a worker thread. `reset_attempt`
    /// resets the backoff counter (fresh add / explicit retry).
    fn spawn_connect(&self, app: &AppHandle, connection: Arc<RemoteConnection>, reset_attempt: bool) {
        if connection.reconnecting.swap(true, Ordering::SeqCst) {
            // A reconnect loop is already running for this scope; let it proceed.
            return;
        }
        if reset_attempt {
            connection.attempt.store(0, Ordering::SeqCst);
        }
        let pool = self.clone();
        let app = app.clone();
        thread::Builder::new()
            .name(format!("hitch-ssh-{}", connection.scope_id))
            .spawn(move || {
                pool.connect_loop(&app, connection.clone());
                connection.reconnecting.store(false, Ordering::SeqCst);
            })
            .expect("failed to spawn ssh connect thread");
    }

    /// Connect with retry+backoff until the scope is connected or removed. Each
    /// failed attempt emits the classified Daemon Status and sleeps a backoff
    /// interval before retrying.
    fn connect_loop(&self, app: &AppHandle, connection: Arc<RemoteConnection>) {
        loop {
            if connection.shutting_down.load(Ordering::SeqCst) {
                return;
            }
            emit_scope_status(app, &connection.scope_id, RemoteStatus::Starting, None);
            match self.connect_once(app, &connection) {
                Ok(()) => {
                    connection.attempt.store(0, Ordering::SeqCst);
                    emit_scope_status(app, &connection.scope_id, RemoteStatus::Running, None);
                    return;
                }
                Err(failure) => {
                    if connection.shutting_down.load(Ordering::SeqCst) {
                        return;
                    }
                    let status = if matches!(failure.outcome, Some(HandshakeOutcome::Hello { .. }))
                        || failure.protocol_mismatch
                    {
                        // A reached-but-incompatible daemon is `failed` with the
                        // classified protocol-mismatch reason (ADR 0014). It will
                        // not get better by retrying, so stop the loop.
                        emit_scope_status(
                            app,
                            &connection.scope_id,
                            RemoteStatus::Failed,
                            Some(failure.message.clone()),
                        );
                        return;
                    } else {
                        RemoteStatus::Unreachable
                    };
                    emit_scope_status(
                        app,
                        &connection.scope_id,
                        status,
                        Some(failure.message.clone()),
                    );
                    let attempt = connection.attempt.fetch_add(1, Ordering::SeqCst) as u32;
                    let delay = backoff_delay(attempt, jitter_unit());
                    // Sleep in small slices so a removal/retry interrupts promptly.
                    if !sleep_interruptible(&connection, delay) {
                        return;
                    }
                }
            }
        }
    }

    /// One connect attempt, orchestrating the approach-C candidate sequence
    /// (ADR 0014 amendment). Spawns ssh, Hello-handshakes over its stdio, and on
    /// success attaches the reader + heartbeat. Returns the classified failure of
    /// the last tried candidate on failure.
    ///
    /// - With a cached `exe_path` (a prior session learned the daemon's binary),
    ///   it launches that path directly. A `MissingHitch` from it means the remote
    ///   moved/updated the binary: clear the cache and re-run the full candidate
    ///   sequence.
    /// - Without a cache, it tries [`candidate_sequence`] in order, falling
    ///   through 1→2 ONLY on `MissingHitch` ([`should_fall_through`]); any other
    ///   category surfaces immediately.
    fn connect_once(
        &self,
        app: &AppHandle,
        connection: &Arc<RemoteConnection>,
    ) -> Result<(), RemoteFailure> {
        // Reconnect fast-path: a learned exe path is launched directly.
        let cached = connection
            .exe_path
            .lock()
            .ok()
            .and_then(|slot| slot.clone());
        if let Some(path) = cached {
            let candidate = ProxyCandidate::CachedExePath(path.clone());
            match self.connect_attempt(app, connection, &candidate) {
                Ok(()) => return Ok(()),
                Err(failure) => {
                    if should_rediscover_cached_path(failure.category) {
                        // Self-healing: the remote moved/updated/removed the binary.
                        // Drop the stale cache and fall back to re-discovery.
                        eprintln!(
                            "hitch: cached remote daemon path `{path}` for {} is stale \
                             (MissingHitch); clearing cache and re-discovering",
                            connection.target
                        );
                        if let Ok(mut slot) = connection.exe_path.lock() {
                            *slot = None;
                        }
                        // Fall through to the candidate sequence below.
                    } else {
                        // Transient/auth/network: keep the cache, surface the error.
                        return Err(failure);
                    }
                }
            }
        }

        // First connect (or post-invalidation re-discovery): try candidates in
        // order, falling through only on MissingHitch.
        let candidates = candidate_sequence();
        let mut last: Option<RemoteFailure> = None;
        for candidate in &candidates {
            match self.connect_attempt(app, connection, candidate) {
                Ok(()) => {
                    eprintln!(
                        "hitch: remote daemon for {} connected via candidate {candidate:?}",
                        connection.target
                    );
                    return Ok(());
                }
                Err(failure) => {
                    let fall_through = should_fall_through(failure.category);
                    eprintln!(
                        "hitch: [ssh-diag] {} candidate {candidate:?} failed (category={:?}, \
                         fall_through={fall_through}): {}",
                        connection.target, failure.category, failure.message
                    );
                    last = Some(failure);
                    if !fall_through {
                        // Auth/HostKey/Network/etc: surface immediately rather than
                        // doubling latency on a retry that can't succeed.
                        break;
                    }
                }
            }
        }
        Err(last.unwrap_or_else(|| RemoteFailure {
            message: format!("no connect candidates for {}", connection.target),
            outcome: None,
            protocol_mismatch: false,
            category: None,
        }))
    }

    /// A single ssh-spawn-and-handshake attempt for one [`ProxyCandidate`]. On a
    /// compatible Hello it takes ownership of the connection (writer/child set,
    /// reader + heartbeat started) and returns `Ok`. Otherwise returns the
    /// classified [`RemoteFailure`].
    fn connect_attempt(
        &self,
        app: &AppHandle,
        connection: &Arc<RemoteConnection>,
        candidate: &ProxyCandidate,
    ) -> Result<(), RemoteFailure> {
        let mut command = Command::new("ssh");
        command
            .arg("-o")
            .arg("BatchMode=yes")
            .arg("-o")
            .arg("ConnectTimeout=10")
            // Keep the proxy and daemon alive across brief network stalls before
            // ssh tears the channel down (so a momentary blip doesn't drop the
            // session) — but bounded so a dead link is detected, not hung forever.
            .arg("-o")
            .arg("ServerAliveInterval=15")
            .arg("-o")
            .arg("ServerAliveCountMax=3")
            .arg("--")
            .arg(&connection.target);
        // The candidate's remote argv (program + `daemon proxy`). The known-location
        // candidate passes `~/.local/bin/hitch` as a LITERAL arg — the remote login
        // shell expands `~`, not the local process.
        for token in candidate.remote_args() {
            command.arg(token);
        }
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        command.creation_flags(CREATE_NO_WINDOW);

        let mut child = command.spawn().map_err(|err| RemoteFailure {
            message: format!(
                "Could not run ssh for {}: {err}. Is OpenSSH installed and on PATH?",
                connection.target
            ),
            outcome: None,
            protocol_mismatch: false,
            category: Some(ssh::FailureCategory::Network),
        })?;

        let mut stdin = child.stdin.take().expect("ssh child stdin piped");
        let stdout = child.stdout.take().expect("ssh child stdout piped");
        let stderr = child.stderr.take();

        // Drain stderr on a worker so a chatty ssh can't deadlock the handshake,
        // and so we have the tail for classification on failure.
        let stderr_buf = Arc::new(Mutex::new(String::new()));
        if let Some(mut pipe) = stderr {
            let sink = stderr_buf.clone();
            thread::spawn(move || {
                let mut s = String::new();
                let _ = pipe.read_to_string(&mut s);
                if let Ok(mut guard) = sink.lock() {
                    *guard = s;
                }
            });
        }

        // Hello handshake over the child's stdio, bounded by a worker + deadline.
        let request_id = connection.next_request_id.fetch_add(1, Ordering::SeqCst);
        let hello = encode_control_message(&ControlMessage::request(
            request_id,
            Request::Hello {
                client_name: "hitch-desktop".into(),
                protocol_version: PROTOCOL_VERSION,
            },
        ))
        .map_err(|err| RemoteFailure {
            message: format!("failed to encode hello: {err}"),
            outcome: None,
            protocol_mismatch: false,
            category: None,
        })?;
        if stdin.write_all(&hello).and_then(|_| stdin.flush()).is_err() {
            let stderr = stderr_buf.lock().map(|s| s.clone()).unwrap_or_default();
            return Err(classify_remote(&connection.target, child, &stderr, HandshakeOutcome::NoResponse));
        }

        let mut reader = BufReader::new(stdout);
        let (hs_tx, hs_rx) = mpsc::channel::<HandshakeResult>();
        // The reader is moved into the worker; on success it is handed back so the
        // event-pump reader can keep reading past the Hello (the daemon replays
        // scrollback immediately after the Hello, ADR 0007). The connection clone
        // lets the worker record the remote daemon's OS family from its Hello.
        let os_family_slot = connection.clone();
        let worker = thread::spawn(move || {
            let outcome = read_remote_hello(&mut reader, request_id, &os_family_slot);
            let _ = hs_tx.send(HandshakeResult { reader, outcome });
        });

        let HandshakeResult { reader, outcome } = match hs_rx.recv_timeout(REMOTE_HANDSHAKE_TIMEOUT) {
            Ok(result) => {
                let _ = worker.join();
                result
            }
            Err(_) => {
                // Deadline: the remote never produced a Hello. Detach the worker
                // (it owns the reader) and classify as a proxy/network failure.
                let stderr = stderr_buf.lock().map(|s| s.clone()).unwrap_or_default();
                let _ = child.kill();
                let status = child.wait().ok();
                let _ = status;
                return Err(classify_remote(
                    &connection.target,
                    spawn_dead_child(),
                    &stderr,
                    HandshakeOutcome::NotAttempted,
                ));
            }
        };

        match outcome {
            HandshakeOutcome::Hello { protocol_version } if protocol_version == PROTOCOL_VERSION => {
                // Success: take ownership of the connection.
                let generation = connection.generation.fetch_add(1, Ordering::SeqCst) + 1;
                *connection.writer.lock().expect("writer lock") = Some(stdin);
                *connection.child.lock().expect("child lock") = Some(child);
                connection.connected.store(true, Ordering::SeqCst);
                self.start_reader(app, connection.clone(), reader, generation);
                self.start_heartbeat(app, connection.clone(), generation);
                Ok(())
            }
            other => {
                let stderr = stderr_buf.lock().map(|s| s.clone()).unwrap_or_default();
                Err(classify_remote(&connection.target, child, &stderr, other))
            }
        }
    }

    /// Spawn the per-scope reader thread: it pumps control responses into the
    /// pending map and re-emits events to the webview, scope-tagged. On EOF/error
    /// it marks the scope unreachable and (unless shutting down) reconnects.
    fn start_reader(
        &self,
        app: &AppHandle,
        connection: Arc<RemoteConnection>,
        reader: BufReader<ChildStdout>,
        generation: u64,
    ) {
        let pool = self.clone();
        let app = app.clone();
        thread::Builder::new()
            .name(format!("hitch-ssh-reader-{}", connection.scope_id))
            .spawn(move || {
                let result = remote_reader_loop(&app, &connection, reader);
                if connection.generation.load(Ordering::SeqCst) == generation {
                    pool.handle_remote_lost(&app, &connection, &result.unwrap_or_default());
                }
            })
            .expect("failed to spawn ssh reader thread");
    }

    /// Spawn the per-scope Ping/Pong heartbeat. A missed Pong marks the scope
    /// unreachable and reconnects (the remote daemon keeps running; only the
    /// proxy link is gone).
    fn start_heartbeat(&self, app: &AppHandle, connection: Arc<RemoteConnection>, generation: u64) {
        let pool = self.clone();
        let app = app.clone();
        thread::Builder::new()
            .name(format!("hitch-ssh-heartbeat-{}", connection.scope_id))
            .spawn(move || loop {
                thread::sleep(REMOTE_HEARTBEAT_INTERVAL);
                if connection.generation.load(Ordering::SeqCst) != generation {
                    return;
                }
                if !connection.is_connected() {
                    return;
                }
                let pong = dispatch_remote_request(
                    &connection,
                    Request::Ping,
                    None,
                    REMOTE_HEARTBEAT_TIMEOUT,
                );
                if connection.generation.load(Ordering::SeqCst) != generation {
                    return;
                }
                if !matches!(pong, Ok(Response::Pong)) {
                    pool.handle_remote_lost(&app, &connection, "remote daemon stopped responding to heartbeat");
                    return;
                }
            })
            .expect("failed to spawn ssh heartbeat thread");
    }

    /// Handle a lost remote link: clear connection state, mark unreachable, and
    /// (unless shutting down) reconnect with backoff.
    fn handle_remote_lost(&self, app: &AppHandle, connection: &Arc<RemoteConnection>, reason: &str) {
        connection.connected.store(false, Ordering::SeqCst);
        connection.generation.fetch_add(1, Ordering::SeqCst);
        if let Ok(mut writer) = connection.writer.lock() {
            *writer = None;
        }
        // Fail any in-flight requests so callers don't hang on a dead link.
        if let Ok(mut pending) = connection.pending.lock() {
            for (_, tx) in pending.drain() {
                let _ = tx.send(Response::Error {
                    error: hitch_proto::ProtocolError::new(
                        hitch_proto::ErrorCode::Unavailable,
                        reason.to_string(),
                    )
                    .retryable(true),
                });
            }
        }
        kill_child(connection);
        if connection.shutting_down.load(Ordering::SeqCst) {
            return;
        }
        emit_scope_status(
            app,
            &connection.scope_id,
            RemoteStatus::Unreachable,
            Some(reason.to_string()),
        );
        // Reconnect with backoff (do NOT reset the attempt counter — a flapping
        // host should back off progressively).
        self.spawn_connect(app, connection.clone(), false);
    }
}

/// The reader/handshake handoff: the worker returns the reader so the event pump
/// can keep reading past the Hello frame.
struct HandshakeResult {
    reader: BufReader<ChildStdout>,
    outcome: HandshakeOutcome,
}

/// A classified remote connect failure: a user-facing message plus the raw
/// handshake outcome (so the loop can decide failed-vs-unreachable) and the
/// classified category (so the candidate orchestrator can decide whether to fall
/// through / re-discover, approach C).
struct RemoteFailure {
    message: String,
    outcome: Option<HandshakeOutcome>,
    protocol_mismatch: bool,
    category: Option<ssh::FailureCategory>,
}

/// Build a `RemoteFailure` from a finished ssh child + stderr + handshake using
/// the shared `ssh::classify` so failure copy matches Test Connection exactly.
fn classify_remote(
    target: &str,
    mut child: Child,
    stderr: &str,
    outcome: HandshakeOutcome,
) -> RemoteFailure {
    let _ = child.kill();
    let exit_code = child.wait().ok().and_then(|s| s.code());
    let result = ssh::classify(target, exit_code, stderr, outcome.clone(), false);
    eprintln!(
        "hitch: [ssh-diag] {target} connect failed — exit={exit_code:?} outcome={outcome:?} \
         category={:?}\n  stderr: {}",
        result.category,
        stderr.trim()
    );
    RemoteFailure {
        message: result.message,
        outcome: Some(outcome.clone()),
        protocol_mismatch: matches!(outcome, HandshakeOutcome::Hello { protocol_version } if protocol_version != PROTOCOL_VERSION),
        category: result.category,
    }
}

/// A placeholder finished child for the deadline path (we already killed the real
/// one). Spawns `true`/a no-op so `classify_remote` has a `Child` to consume.
fn spawn_dead_child() -> Child {
    // The classifier only reads the exit code; a process that has already exited
    // gives None, which the deadline branch wants anyway.
    #[cfg(unix)]
    let mut cmd = Command::new("true");
    #[cfg(windows)]
    let mut cmd = {
        let mut c = Command::new("cmd");
        c.arg("/c").arg("exit").arg("0");
        c
    };
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd.spawn().expect("spawn placeholder child")
}

/// Read newline-delimited control JSON from the remote until the Hello response
/// for `request_id` arrives, skipping non-matching frames. Mirrors `ssh::read_hello`
/// but takes a generic `BufRead`.
fn read_remote_hello<R: BufRead>(
    reader: &mut R,
    request_id: RequestId,
    connection: &Arc<RemoteConnection>,
) -> HandshakeOutcome {
    loop {
        match read_control_message(reader) {
            Ok(Some(ControlMessage::Response {
                id,
                response:
                    Response::Hello {
                        protocol_version,
                        os_family,
                        exe_path,
                        ..
                    },
            })) if id == request_id => {
                // Record the remote daemon's platform so inserted upload paths get
                // remote-appropriate quoting (issue #31).
                if let Ok(mut slot) = connection.os_family.lock() {
                    *slot = os_family;
                }
                // Cache the daemon's own exe path so reconnects launch it directly
                // (approach C). Only cache a compatible Hello — a version-mismatched
                // daemon won't be used. `None` (old daemon) leaves the cache as-is.
                if protocol_version == PROTOCOL_VERSION {
                    if let (Ok(mut slot), Some(path)) =
                        (connection.exe_path.lock(), exe_path.as_ref())
                    {
                        *slot = Some(path.clone());
                    }
                }
                return HandshakeOutcome::Hello { protocol_version };
            }
            Ok(Some(_)) => {}
            Ok(None) => return HandshakeOutcome::NoResponse,
            Err(_) => return HandshakeOutcome::Malformed,
        }
    }
}

/// The per-scope reader loop: pumps responses into the pending map and re-emits
/// events to the webview as scope-tagged `hitch-scope-event`. PTY output bytes
/// route into this scope's output router. Mirrors `lib.rs::reader_loop`.
fn remote_reader_loop(
    app: &AppHandle,
    connection: &Arc<RemoteConnection>,
    mut reader: BufReader<ChildStdout>,
) -> Result<String, String> {
    loop {
        let message = match read_control_message(&mut reader) {
            Ok(Some(message)) => message,
            Ok(None) => return Ok("remote daemon closed the proxy stream".to_string()),
            Err(err) => return Ok(format!("remote daemon read error: {err}")),
        };
        match message {
            ControlMessage::Response { id, response } => {
                if let Ok(mut pending) = connection.pending.lock() {
                    if let Some(tx) = pending.remove(&id) {
                        let _ = tx.send(response);
                    }
                }
            }
            ControlMessage::Event { event } => {
                if let Event::SessionOutput { session_id, byte_count } = &event {
                    let payload = match read_pty_payload(&mut reader) {
                        Ok(payload) => payload,
                        Err(err) => return Ok(format!("remote PTY read error: {err}")),
                    };
                    if payload.len() != *byte_count as usize {
                        return Ok("remote PTY frame length mismatch".to_string());
                    }
                    if let Ok(mut router) = connection.output_router.lock() {
                        router.send_or_stage(*session_id, payload);
                    }
                    continue;
                }
                // Keep the output router's opened/closed bookkeeping in step so a
                // reconnect replay registers a fresh channel rather than racing a
                // stale one (mirrors the local reader's SessionOpened/Closed prep).
                match &event {
                    Event::SessionOpened { session, .. } => {
                        if let Ok(mut router) = connection.output_router.lock() {
                            router.prepare_fresh_registration(session.id);
                        }
                    }
                    Event::SessionClosed { session_id, .. } => {
                        if let Ok(mut router) = connection.output_router.lock() {
                            router.close_session(*session_id);
                        }
                    }
                    _ => {}
                }
                let _ = app.emit(
                    "hitch-scope-event",
                    ScopeEventPayload {
                        scope: &connection.scope_id,
                        event: &event,
                    },
                );
            }
            ControlMessage::Request { .. } => {
                // The daemon never sends requests to the client.
            }
        }
    }
}

/// Dispatch one request over a remote connection, blocking for its response.
fn dispatch_remote_request(
    connection: &Arc<RemoteConnection>,
    request: Request,
    pty_payload: Option<Vec<u8>>,
    timeout: Duration,
) -> Result<Response, String> {
    let request_id = connection.next_request_id.fetch_add(1, Ordering::SeqCst);
    let (tx, rx) = mpsc::channel();
    connection
        .pending
        .lock()
        .map_err(|_| "remote pending lock poisoned".to_string())?
        .insert(request_id, tx);

    let write_result = (|| -> Result<(), String> {
        let mut writer_guard = connection
            .writer
            .lock()
            .map_err(|_| "remote writer lock poisoned".to_string())?;
        let writer = writer_guard
            .as_mut()
            .ok_or_else(|| "remote daemon is not connected".to_string())?;
        let control = encode_control_message(&ControlMessage::request(request_id, request))
            .map_err(|err| err.to_string())?;
        writer
            .write_all(&control)
            .map_err(|err| format!("failed to send remote request: {err}"))?;
        if let Some(payload) = pty_payload.as_deref() {
            let frame = encode_pty_frame(payload).map_err(|err| err.to_string())?;
            writer
                .write_all(&frame)
                .map_err(|err| format!("failed to send remote PTY input: {err}"))?;
        }
        writer
            .flush()
            .map_err(|err| format!("failed to flush remote request: {err}"))?;
        Ok(())
    })();

    if let Err(err) = write_result {
        if let Ok(mut pending) = connection.pending.lock() {
            pending.remove(&request_id);
        }
        return Err(err);
    }

    match rx.recv_timeout(timeout) {
        Ok(response) => Ok(response),
        Err(mpsc::RecvTimeoutError::Timeout) => {
            if let Ok(mut pending) = connection.pending.lock() {
                pending.remove(&request_id);
            }
            Err("timed out waiting for remote daemon response".into())
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            Err("remote daemon response channel disconnected".into())
        }
    }
}

/// Stream one local file to the remote daemon (issue #31). Returns:
/// - `Ok(Some(Uploaded|RejectedDirectory))` for a completed file or a directory
///   rejection (a non-fatal per-file outcome the batch keeps going past),
/// - `Ok(None)` when the in-flight file was cancelled (the batch must stop and
///   insert nothing),
/// - `Err(Failed)` for a local read error on this file (the batch keeps going).
#[allow(clippy::too_many_arguments)]
fn upload_one_file(
    app: &AppHandle,
    connection: &Arc<RemoteConnection>,
    batch_id: &str,
    session_id: SessionId,
    path: &Path,
    file_index: usize,
    file_count: usize,
    cancel: &Arc<AtomicBool>,
) -> Result<Option<UploadFileResult>, UploadFileResult> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned());

    let metadata = std::fs::metadata(path).map_err(|err| UploadFileResult::Failed {
        name: name.clone(),
        error: format!("could not read {}: {err}", path.display()),
    })?;
    if metadata.is_dir() {
        // Recursive upload isn't supported in the first design (ADR 0014): reject
        // the directory but keep the batch going for the regular files.
        return Ok(Some(UploadFileResult::RejectedDirectory { name }));
    }
    let total_bytes = metadata.len();

    let mut file = std::fs::File::open(path).map_err(|err| UploadFileResult::Failed {
        name: name.clone(),
        error: format!("could not open {}: {err}", path.display()),
    })?;

    // Begin the upload (daemon reserves a collision-suffixed final name).
    let begin = dispatch_remote_request(
        connection,
        Request::BeginUpload {
            session_id,
            file_name: name.clone(),
            total_bytes,
        },
        None,
        REMOTE_REQUEST_TIMEOUT,
    )
    .map_err(|err| UploadFileResult::Failed {
        name: name.clone(),
        error: err,
    })?;
    let upload_id = match begin {
        Response::UploadStarted { upload_id, .. } => upload_id,
        Response::Error { error } => {
            return Err(UploadFileResult::Failed {
                name,
                error: error.message,
            });
        }
        other => {
            return Err(UploadFileResult::Failed {
                name,
                error: format!("unexpected begin-upload response: {other:?}"),
            });
        }
    };

    // Stream the bytes in bounded chunks, one request/response turn each so PTY
    // traffic interleaves. Emit progress and honour cancellation between chunks.
    let mut sent: u64 = 0;
    let mut buf = vec![0u8; UPLOAD_CHUNK_BYTES];
    emit_upload_progress(app, batch_id, file_index, file_count, &name, sent, total_bytes);
    loop {
        if cancel.load(Ordering::SeqCst) {
            let _ = dispatch_remote_request(
                connection,
                Request::AbortUpload {
                    upload_id: upload_id.clone(),
                },
                None,
                REMOTE_REQUEST_TIMEOUT,
            );
            return Ok(None);
        }
        let read = match file.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(err) => {
                let _ = dispatch_remote_request(
                    connection,
                    Request::AbortUpload {
                        upload_id: upload_id.clone(),
                    },
                    None,
                    REMOTE_REQUEST_TIMEOUT,
                );
                return Err(UploadFileResult::Failed {
                    name,
                    error: format!("read error on {}: {err}", path.display()),
                });
            }
        };
        let ack = dispatch_remote_request(
            connection,
            Request::UploadChunk {
                upload_id: upload_id.clone(),
                byte_count: read as u32,
            },
            Some(buf[..read].to_vec()),
            REMOTE_REQUEST_TIMEOUT,
        );
        match ack {
            Ok(Response::Ack) => {}
            Ok(Response::Error { error }) => {
                return Err(UploadFileResult::Failed {
                    name,
                    error: error.message,
                });
            }
            Ok(other) => {
                return Err(UploadFileResult::Failed {
                    name,
                    error: format!("unexpected chunk response: {other:?}"),
                });
            }
            Err(err) => {
                return Err(UploadFileResult::Failed { name, error: err });
            }
        }
        sent += read as u64;
        emit_upload_progress(app, batch_id, file_index, file_count, &name, sent, total_bytes);
    }

    // Finish: the daemon returns the actual final remote path to insert.
    let finish = dispatch_remote_request(
        connection,
        Request::FinishUpload {
            upload_id: upload_id.clone(),
        },
        None,
        REMOTE_REQUEST_TIMEOUT,
    )
    .map_err(|err| UploadFileResult::Failed {
        name: name.clone(),
        error: err,
    })?;
    match finish {
        Response::UploadFinished { remote_path } => {
            Ok(Some(UploadFileResult::Uploaded { name, remote_path }))
        }
        Response::Error { error } => Err(UploadFileResult::Failed {
            name,
            error: error.message,
        }),
        other => Err(UploadFileResult::Failed {
            name,
            error: format!("unexpected finish-upload response: {other:?}"),
        }),
    }
}

/// Emit one `hitch-upload-progress` tick to the webview.
fn emit_upload_progress(
    app: &AppHandle,
    batch_id: &str,
    file_index: usize,
    file_count: usize,
    file_name: &str,
    sent_bytes: u64,
    total_bytes: u64,
) {
    let _ = app.emit(
        "hitch-upload-progress",
        UploadProgress {
            batch_id,
            file_index,
            file_count,
            file_name,
            sent_bytes,
            total_bytes,
        },
    );
}

/// Write one fire-and-forget input frame to a remote connection (best-effort).
fn write_remote_input(connection: &Arc<RemoteConnection>, session_id: SessionId, bytes: &[u8]) {
    let Ok(frame) = encode_pty_frame(bytes) else {
        return;
    };
    let request_id = connection.next_request_id.fetch_add(1, Ordering::SeqCst);
    let request = Request::SendSessionInput {
        session_id,
        byte_count: bytes.len() as u32,
    };
    let Ok(mut writer_guard) = connection.writer.lock() else {
        return;
    };
    let Some(writer) = writer_guard.as_mut() else {
        return;
    };
    let Ok(control) = encode_control_message(&ControlMessage::request(request_id, request)) else {
        return;
    };
    if writer.write_all(&control).is_err() {
        return;
    }
    if writer.write_all(&frame).is_err() {
        return;
    }
    let _ = writer.flush();
}

/// Kill a remote connection's SSH child if one is held.
fn kill_child(connection: &Arc<RemoteConnection>) {
    if let Ok(mut child) = connection.child.lock() {
        if let Some(mut child) = child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Sleep `delay`, but in small slices so a removal/retry interrupts promptly.
/// Returns false if the connection was torn down during the sleep.
fn sleep_interruptible(connection: &Arc<RemoteConnection>, delay: Duration) -> bool {
    let slice = Duration::from_millis(200);
    let mut remaining = delay;
    while remaining > Duration::ZERO {
        if connection.shutting_down.load(Ordering::SeqCst) {
            return false;
        }
        let step = remaining.min(slice);
        thread::sleep(step);
        remaining = remaining.saturating_sub(step);
    }
    !connection.shutting_down.load(Ordering::SeqCst)
}

/// Emit a per-scope Daemon Status to the webview.
fn emit_scope_status(app: &AppHandle, scope_id: &str, status: RemoteStatus, reason: Option<String>) {
    let _ = app.emit(
        "hitch-scope-status",
        ScopeStatusPayload {
            scope: scope_id,
            status,
            reason,
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_schedule_matches_adr() {
        assert_eq!(backoff_base_secs(0), 2);
        assert_eq!(backoff_base_secs(1), 5);
        assert_eq!(backoff_base_secs(2), 15);
        assert_eq!(backoff_base_secs(3), 30);
        assert_eq!(backoff_base_secs(4), 60);
        // Capped at ~60s for every later attempt.
        assert_eq!(backoff_base_secs(9), 60);
        assert_eq!(backoff_base_secs(100), 60);
    }

    #[test]
    fn backoff_delay_applies_bounded_jitter() {
        // At rand 0.0 the factor is 0.8; at ~1.0 it approaches 1.2. The delay is
        // always within [0.8x, 1.2x] of the base and at least 1s.
        for attempt in 0..6u32 {
            let base = backoff_base_secs(attempt) as f64;
            let low = backoff_delay(attempt, 0.0).as_secs_f64();
            let high = backoff_delay(attempt, 0.999).as_secs_f64();
            assert!(low >= (base * 0.8).max(1.0) - 0.001, "attempt {attempt} low {low}");
            assert!(high <= base * 1.2 + 0.001, "attempt {attempt} high {high}");
            assert!(low <= high, "attempt {attempt}: low {low} > high {high}");
            assert!(low >= 1.0, "attempt {attempt}: below 1s floor");
        }
    }

    #[test]
    fn backoff_delay_floors_first_attempt_at_one_second() {
        // The 2s base * 0.8 jitter = 1.6s, still above the 1s floor; a smaller
        // base would clamp. Verify the floor explicitly via a synthetic call.
        let d = backoff_delay(0, 0.0);
        assert!(d >= Duration::from_secs(1));
    }

    // ----- Approach C: candidate orchestration (pure decision logic) -----

    use ssh::FailureCategory;

    #[test]
    fn candidate_sequence_is_known_location_then_bare() {
        // First connect tries the Unix self-install location, then bare `hitch`.
        let seq = candidate_sequence();
        assert_eq!(
            seq,
            vec![ProxyCandidate::KnownLocation, ProxyCandidate::BarePath]
        );
    }

    #[test]
    fn known_location_passes_tilde_as_literal_arg() {
        // `~` is NOT locally expanded — it's a literal argv element the remote
        // login shell expands. The program token is the first remote arg.
        let args = ProxyCandidate::KnownLocation.remote_args();
        assert_eq!(args, vec!["~/.local/bin/hitch", "daemon", "proxy"]);
    }

    #[test]
    fn bare_path_candidate_uses_bare_hitch() {
        let args = ProxyCandidate::BarePath.remote_args();
        assert_eq!(args, vec!["hitch", "daemon", "proxy"]);
    }

    #[test]
    fn cached_exe_path_candidate_launches_that_path_directly() {
        // Reconnect launches the learned absolute path directly (no `~`, no PATH).
        let args =
            ProxyCandidate::CachedExePath("/opt/hitch/bin/hitch".into()).remote_args();
        assert_eq!(args, vec!["/opt/hitch/bin/hitch", "daemon", "proxy"]);
    }

    #[test]
    fn fall_through_on_program_specific_failures() {
        // Known-path → bare fall-through happens for failures specific to the
        // program we invoked: the binary wasn't there (MissingHitch) OR it ran but
        // produced no Hitch stream (ProxyStartup — the bucket a Windows remote's
        // "not recognized"/"not found" wording lands in for `~/.local/bin/hitch`).
        assert!(should_fall_through(Some(FailureCategory::MissingHitch)));
        assert!(should_fall_through(Some(FailureCategory::ProxyStartup)));
    }

    #[test]
    fn host_level_failures_surface_immediately_no_fall_through() {
        // Host-level failures are identical for every candidate, so retrying on the
        // next candidate just doubles latency.
        assert!(!should_fall_through(Some(FailureCategory::Auth)));
        assert!(!should_fall_through(Some(FailureCategory::HostKey)));
        assert!(!should_fall_through(Some(FailureCategory::Network)));
        // A reached-but-wrong-version hitch is terminal — report it, don't mask it
        // by falling through.
        assert!(!should_fall_through(Some(FailureCategory::ProtocolMismatch)));
        // No classified category (spawn/encode error) also surfaces.
        assert!(!should_fall_through(None));
    }

    #[test]
    fn stale_cached_path_rediscovers_only_on_missing_hitch() {
        // A MissingHitch from a cached path means the remote moved/updated the
        // binary → clear cache + re-run the candidate sequence.
        assert!(should_rediscover_cached_path(Some(FailureCategory::MissingHitch)));
        // Every other category is transient/auth/network → keep the cache and let
        // normal backoff handle it.
        assert!(!should_rediscover_cached_path(Some(FailureCategory::Auth)));
        assert!(!should_rediscover_cached_path(Some(FailureCategory::HostKey)));
        assert!(!should_rediscover_cached_path(Some(FailureCategory::Network)));
        assert!(!should_rediscover_cached_path(Some(FailureCategory::ProxyStartup)));
        assert!(!should_rediscover_cached_path(None));
    }

    /// Simulate the first-connect candidate walk over a sequence of per-candidate
    /// outcome categories, applying the pure fall-through rule. Returns the index
    /// of the winning candidate (`Ok`) or the index of the candidate whose failure
    /// was surfaced (`Err`). This mirrors `connect_once`'s loop without live SSH.
    fn walk_candidates(outcomes: &[Option<FailureCategory>]) -> Result<usize, usize> {
        let seq = candidate_sequence();
        let mut last_idx = 0;
        for (idx, _candidate) in seq.iter().enumerate() {
            let category = outcomes.get(idx).copied().flatten();
            // `None` here in the test harness means "this candidate succeeded".
            if outcomes.get(idx).map(|c| c.is_none()).unwrap_or(false) {
                return Ok(idx);
            }
            last_idx = idx;
            if !should_fall_through(category) {
                return Err(idx);
            }
        }
        Err(last_idx)
    }

    #[test]
    fn known_path_missing_then_bare_succeeds() {
        // Candidate 0 (known location) MissingHitch → fall through; candidate 1
        // (bare) succeeds.
        let outcome = walk_candidates(&[Some(FailureCategory::MissingHitch), None]);
        assert_eq!(outcome, Ok(1));
    }

    #[test]
    fn known_path_proxy_startup_then_bare_succeeds() {
        // The real-world Windows case: `~/.local/bin/hitch` isn't a runnable program
        // on the remote, so the candidate classifies as ProxyStartup (its shell's
        // "not recognized"/"not found" wording doesn't match the POSIX MissingHitch
        // patterns). It must fall through to bare `hitch` (on the registry PATH),
        // which is what Test Connection runs and what succeeds.
        let outcome = walk_candidates(&[Some(FailureCategory::ProxyStartup), None]);
        assert_eq!(outcome, Ok(1));
    }

    #[test]
    fn known_path_auth_surfaces_without_trying_bare() {
        // Candidate 0 Auth → surface immediately, candidate 1 is never tried.
        let outcome = walk_candidates(&[Some(FailureCategory::Auth), None]);
        assert_eq!(outcome, Err(0));
    }

    #[test]
    fn both_candidates_missing_surfaces_last() {
        // Both MissingHitch → the bare candidate's failure is the surfaced one.
        let outcome = walk_candidates(&[
            Some(FailureCategory::MissingHitch),
            Some(FailureCategory::MissingHitch),
        ]);
        assert_eq!(outcome, Err(1));
    }

    #[test]
    fn known_path_succeeds_first() {
        let outcome = walk_candidates(&[None, None]);
        assert_eq!(outcome, Ok(0));
    }

    /// The cached-path reconnect decision, expressed as the pure choice the
    /// orchestrator makes: given a cached path's failure category, do we re-run
    /// the candidate sequence (re-discovery) or surface? This mirrors
    /// `connect_once`'s reconnect fast-path without live SSH.
    fn reconnect_decision(cached_failure: Option<FailureCategory>) -> ReconnectChoice {
        if should_rediscover_cached_path(cached_failure) {
            ReconnectChoice::Rediscover
        } else {
            ReconnectChoice::Surface
        }
    }

    #[derive(Debug, PartialEq, Eq)]
    enum ReconnectChoice {
        Rediscover,
        Surface,
    }

    #[test]
    fn cached_path_used_then_stale_triggers_rediscovery() {
        // A stale cached path (MissingHitch) re-discovers via the candidate walk.
        assert_eq!(
            reconnect_decision(Some(FailureCategory::MissingHitch)),
            ReconnectChoice::Rediscover
        );
        // And the subsequent candidate walk resolves to bare on the second try.
        let outcome = walk_candidates(&[Some(FailureCategory::MissingHitch), None]);
        assert_eq!(outcome, Ok(1));
    }

    #[test]
    fn cached_path_transient_failure_keeps_cache() {
        // A network blip on the cached path does NOT re-discover; backoff retries
        // the same cached path.
        assert_eq!(
            reconnect_decision(Some(FailureCategory::Network)),
            ReconnectChoice::Surface
        );
    }
}
