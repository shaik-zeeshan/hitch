//! `hitch-desktop` (src-tauri) — the thin Tauri client (ADR 0005).
//!
//! The GUI process holds no git/pty/store/agent logic: it only keeps a daemon
//! socket connection, starts the daemon when needed, and relays `hitch-proto`
//! requests/responses/events to Tauri IPC.

use std::collections::{HashMap, HashSet};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
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

/// Per-Session webview channel registry, with staging to absorb the Tauri-side
/// channel-registration round-trip (ADR 0007, ADR 0008).
///
/// Lives in the Tauri process. Deliberately not a peer of the daemon's
/// `OutputBroadcaster`: the broadcaster owns the dispatcher-thread FIFO between
/// replay snapshots and live bytes, while this registry owns the gap between
/// the daemon emitting bytes for a Session and the webview handing a binary
/// `Channel<&[u8]>` to the Tauri process via `invoke('register_session_output')`.
/// See ADR 0008 for why these are not unified behind a single interface.
///
/// Channels are kept across an ordinary daemon disconnect, but a `SessionOpened`
/// replay invalidates that session's channel until the webview has reset its byte
/// ring and registered a fresh channel; replay bytes are staged during that
/// registration gap.
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
struct WebviewChannelRegistry {
    channels: HashMap<SessionId, Channel<InvokeResponseBody>>,
    /// Bytes that arrived before the channel was registered, per session.
    staging: HashMap<SessionId, Vec<u8>>,
    /// Sessions that have already produced a SessionOpened event in this GUI
    /// process. A later SessionOpened for the same id is a reconnect/replay and
    /// should discard pre-replay staging; the first SessionOpened must preserve
    /// bytes that raced ahead of the event.
    opened_sessions: HashSet<SessionId>,
}

impl WebviewChannelRegistry {
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
    channel_registry: Mutex<WebviewChannelRegistry>,
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
            channel_registry: Mutex::new(WebviewChannelRegistry::default()),
        }))
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
                self.spawn_daemon()?;
                self.wait_for_daemon()?
            }
        };
        self.attach_stream(app, stream)
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
        Ok(())
    }

    fn restart_daemon(&self, app: &AppHandle, reason: String) -> Result<(), String> {
        self.request_daemon_shutdown();
        self.mark_disconnected(app, reason);

        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if UnixStream::connect(&self.0.socket_path).is_err() {
                break;
            }
            thread::sleep(Duration::from_millis(100));
        }

        self.spawn_daemon()?;
        let stream = self.wait_for_daemon()?;
        self.attach_stream(app, stream)
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
            self.mark_disconnected(app, err.clone());
            return Err(err);
        }

        // Fixed client-side response deadline. The daemon clamps its
        // configurable draft timeout safely below this (see
        // `hitch-daemon`'s `drafts::MAX_TIMEOUT_SECS`, currently 120 - 10s
        // margin) so a slow draft still produces a daemon response — success
        // or timeout error — before the client abandons the request and the
        // reader_loop drops the late reply. Keep these two values in sync.
        match rx.recv_timeout(Duration::from_secs(120)) {
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
                        client.mark_disconnected(&app, err.to_string());
                    }
                }
            })
            .expect("failed to spawn daemon reader thread");
    }

    fn mark_disconnected(&self, app: &AppHandle, reason: String) {
        self.0.connected.store(false, Ordering::SeqCst);
        if let Ok(mut writer) = self.0.writer.lock() {
            *writer = None;
        }
        if let Ok(mut sessions) = self.0.sessions.lock() {
            sessions.clear();
        }
        self.refresh_tray(app);

        let error_response = Response::Error {
            error: ProtocolError::new(ErrorCode::Unavailable, reason.clone()).retryable(true),
        };
        if let Ok(mut pending) = self.0.pending.lock() {
            for (_, tx) in pending.drain() {
                let _ = tx.send(error_response.clone());
            }
        }

        let _ = app.emit("hitch-disconnected", DisconnectedPayload { reason });
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
        let text = tray_status_text(self.is_connected(), count);
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
    /// the Ack: the caller is about to exit the GUI process.
    fn request_daemon_shutdown(&self) {
        if !self.is_connected() {
            return;
        }
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
            let output = Command::new(&path)
                .arg("--socket")
                .arg(&self.0.socket_path)
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
        Err(format!(
            "daemon did not become ready at {}: {}",
            self.0.socket_path.display(),
            last_error
                .map(|err| err.to_string())
                .unwrap_or_else(|| "unknown error".into())
        ))
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
                        if let Ok(mut registry) = client.0.channel_registry.lock() {
                            registry.send_or_stage(*session_id, payload);
                        }
                        continue;
                    }
                    Event::SessionOpened { session } => {
                        if let Ok(mut registry) = client.0.channel_registry.lock() {
                            registry.prepare_fresh_registration(session.id);
                        }
                        client.track_session(app, session.id, true)
                    }
                    Event::SessionClosed { session_id, .. } => {
                        if let Ok(mut registry) = client.0.channel_registry.lock() {
                            registry.close_session(*session_id);
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

#[derive(Debug, Clone, Serialize)]
struct DisconnectedPayload {
    reason: String,
}

#[tauri::command]
async fn connect_daemon(app: AppHandle, state: State<'_, HitchClient>) -> Result<(), String> {
    let client = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        client.connect(&app)?;
        match client.send_request(
            &app,
            Request::Hello {
                client_name: "hitch-desktop".into(),
                protocol_version: PROTOCOL_VERSION,
            },
        )? {
            Response::Hello { .. } => Ok(()),
            // Any Hello error means the running daemon is incompatible — restart
            // regardless of error code, since old daemons may serialize error codes
            // differently (e.g. protocol v2 may use a variant unknown to this client).
            Response::Error { error } => {
                client.restart_daemon(
                    &app,
                    format!("restarting incompatible daemon: {}", error.message),
                )?;
                match client.send_request(
                    &app,
                    Request::Hello {
                        client_name: "hitch-desktop".into(),
                        protocol_version: PROTOCOL_VERSION,
                    },
                )? {
                    Response::Hello { .. } => Ok(()),
                    Response::Error { error } => Err(error.message),
                    other => Err(format!(
                        "unexpected hello response after daemon restart: {other:?}"
                    )),
                }
            }
            other => Err(format!("unexpected hello response: {other:?}")),
        }
    })
    .await
    .map_err(|err| format!("daemon connection task failed: {err}"))?
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
    let mut registry = state
        .0
        .channel_registry
        .lock()
        .map_err(|_| "channel-registry lock poisoned".to_string())?;
    registry.register_channel(session_id, channel);
    Ok(())
}

/// Drop a session's output channel + any staged bytes (ADR 0007). Called when
/// the session closes or the webview tears its terminal down.
#[tauri::command]
fn unregister_session_output(
    state: State<'_, HitchClient>,
    session_id: SessionId,
) -> Result<(), String> {
    let mut registry = state
        .0
        .channel_registry
        .lock()
        .map_err(|_| "channel-registry lock poisoned".to_string())?;
    registry.unregister_channel(session_id);
    Ok(())
}

/// Menu-bar status line, e.g. "Hitch — running 2 sessions". The daemon keeps
/// running after the window closes, so this is the honest signal that Hitch has
/// a background presence (ADR 0003).
fn tray_status_text(connected: bool, count: usize) -> String {
    if !connected {
        "Hitch — daemon offline".to_string()
    } else {
        match count {
            0 => "Hitch — no active sessions".to_string(),
            1 => "Hitch — running 1 session".to_string(),
            n => format!("Hitch — running {n} sessions"),
        }
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
        tray_status_text(false, 0),
        false,
        None::<&str>,
    )?;
    let show = MenuItem::with_id(app, "show", "Show Hitch", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit Hitch", true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[&status, &PredefinedMenuItem::separator(app)?, &show, &quit],
    )?;

    let builder = TrayIconBuilder::with_id(TRAY_ID)
        .menu(&menu)
        .tooltip(tray_status_text(false, 0))
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
    use super::{tray_status_text, WebviewChannelRegistry};
    use hitch_core::SessionId;
    use std::sync::{Arc, Mutex};
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
        let mut registry = WebviewChannelRegistry::default();

        registry.send_or_stage(session_id, b"early-prompt".to_vec());
        registry.prepare_fresh_registration(session_id);

        assert_eq!(
            registry.staging.get(&session_id).cloned(),
            Some(b"early-prompt".to_vec())
        );

        registry.register_channel(session_id, recording_channel(received.clone()));
        assert_eq!(
            received.lock().unwrap().clone(),
            vec![b"early-prompt".to_vec()]
        );
        assert!(!registry.staging.contains_key(&session_id));
    }

    #[test]
    fn session_opened_stages_replay_until_fresh_channel_registered() {
        let session_id = SessionId::new();
        let old_received = Arc::new(Mutex::new(Vec::new()));
        let new_received = Arc::new(Mutex::new(Vec::new()));
        let mut registry = WebviewChannelRegistry::default();

        registry.prepare_fresh_registration(session_id);
        registry.register_channel(session_id, recording_channel(old_received.clone()));
        registry.send_or_stage(session_id, b"before-reconnect".to_vec());
        assert_eq!(
            old_received.lock().unwrap().clone(),
            vec![b"before-reconnect".to_vec()]
        );

        registry.prepare_fresh_registration(session_id);
        registry.send_or_stage(session_id, b"replayed-scrollback".to_vec());

        assert_eq!(
            old_received.lock().unwrap().clone(),
            vec![b"before-reconnect".to_vec()],
            "replay must not be delivered over the stale pre-reconnect channel"
        );
        assert_eq!(
            registry.staging.get(&session_id).cloned(),
            Some(b"replayed-scrollback".to_vec())
        );

        registry.register_channel(session_id, recording_channel(new_received.clone()));
        assert_eq!(
            new_received.lock().unwrap().clone(),
            vec![b"replayed-scrollback".to_vec()]
        );
        assert!(!registry.staging.contains_key(&session_id));

        registry.send_or_stage(session_id, b"live-after-registration".to_vec());
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
        let mut registry = WebviewChannelRegistry::default();

        registry.prepare_fresh_registration(session_id);
        registry.send_or_stage(session_id, b"missed-before-reconnect".to_vec());
        assert!(registry.staging.contains_key(&session_id));

        registry.prepare_fresh_registration(session_id);
        assert!(!registry.staging.contains_key(&session_id));
    }

    #[test]
    fn registration_flush_failure_restages_staged_bytes() {
        let session_id = SessionId::new();
        let received = Arc::new(Mutex::new(Vec::new()));
        let mut registry = WebviewChannelRegistry::default();

        registry.send_or_stage(session_id, b"early-prompt".to_vec());

        registry.register_channel(
            session_id,
            Channel::new(|_| Err(std::io::Error::other("closed channel").into())),
        );

        assert!(!registry.channels.contains_key(&session_id));
        assert_eq!(
            registry.staging.get(&session_id).cloned(),
            Some(b"early-prompt".to_vec())
        );

        registry.register_channel(session_id, recording_channel(received.clone()));
        assert_eq!(
            received.lock().unwrap().clone(),
            vec![b"early-prompt".to_vec()]
        );
        assert!(!registry.staging.contains_key(&session_id));
    }

    #[test]
    fn send_failure_removes_dead_channel_and_stages_payload() {
        let session_id = SessionId::new();
        let received = Arc::new(Mutex::new(Vec::new()));
        let mut registry = WebviewChannelRegistry::default();

        registry.prepare_fresh_registration(session_id);
        registry.register_channel(
            session_id,
            Channel::new(|_| Err(std::io::Error::other("closed channel").into())),
        );

        registry.send_or_stage(session_id, b"not-lost".to_vec());

        assert!(!registry.channels.contains_key(&session_id));
        assert_eq!(
            registry.staging.get(&session_id).cloned(),
            Some(b"not-lost".to_vec())
        );

        registry.register_channel(session_id, recording_channel(received.clone()));
        assert_eq!(received.lock().unwrap().clone(), vec![b"not-lost".to_vec()]);
        assert!(!registry.staging.contains_key(&session_id));
    }

    #[test]
    fn session_closed_forgets_opened_session_state() {
        let session_id = SessionId::new();
        let mut registry = WebviewChannelRegistry::default();

        registry.prepare_fresh_registration(session_id);
        registry.close_session(session_id);
        registry.send_or_stage(session_id, b"new-early-prompt".to_vec());
        registry.prepare_fresh_registration(session_id);

        assert_eq!(
            registry.staging.get(&session_id).cloned(),
            Some(b"new-early-prompt".to_vec())
        );
    }

    #[test]
    fn tray_status_text_reflects_connection_and_count() {
        assert_eq!(tray_status_text(false, 0), "Hitch — daemon offline");
        assert_eq!(tray_status_text(false, 3), "Hitch — daemon offline");
        assert_eq!(tray_status_text(true, 0), "Hitch — no active sessions");
        assert_eq!(tray_status_text(true, 1), "Hitch — running 1 session");
        assert_eq!(tray_status_text(true, 4), "Hitch — running 4 sessions");
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
            unregister_session_output
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
