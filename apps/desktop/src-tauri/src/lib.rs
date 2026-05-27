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
use tauri::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Manager, State, WindowEvent, Wry};

#[derive(Clone)]
struct HitchClient(Arc<HitchClientInner>);

struct HitchClientInner {
    socket_path: PathBuf,
    next_request_id: AtomicU64,
    connected: AtomicBool,
    connect_lock: Mutex<()>,
    writer: Mutex<Option<UnixStream>>,
    pending: Mutex<HashMap<RequestId, mpsc::Sender<Response>>>,
    /// Live sessions, mirrored from daemon session-opened/closed events so the
    /// menu-bar tray can show how many sessions are still running.
    sessions: Mutex<HashSet<SessionId>>,
    /// The tray's status line; populated once the tray is built in `setup`.
    tray_status: Mutex<Option<MenuItem<Wry>>>,
}

/// The tray's stable id, used to look it up for tooltip updates.
const TRAY_ID: &str = "hitch-tray";

impl HitchClient {
    fn new() -> Self {
        Self(Arc::new(HitchClientInner {
            socket_path: hitch_proto::transport::default_socket_path(),
            next_request_id: AtomicU64::new(1),
            connected: AtomicBool::new(false),
            connect_lock: Mutex::new(()),
            writer: Mutex::new(None),
            pending: Mutex::new(HashMap::new()),
            sessions: Mutex::new(HashSet::new()),
            tray_status: Mutex::new(None),
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

        self.start_reader(app.clone(), stream);
        Ok(())
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

    fn start_reader(&self, app: AppHandle, stream: UnixStream) {
        let client = self.clone();
        thread::Builder::new()
            .name("hitch-daemon-reader".into())
            .spawn(move || {
                let result = reader_loop(&app, &client, stream);
                if let Err(err) = result {
                    client.mark_disconnected(&app, err.to_string());
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
        let Ok(bytes) =
            encode_control_message(&ControlMessage::request(request_id, Request::ShutdownDaemon))
        else {
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
                        app.emit(
                            "hitch-session-output",
                            SessionOutputPayload {
                                session_id: *session_id,
                                data: String::from_utf8_lossy(&payload).into_owned(),
                            },
                        )
                        .map_err(io::Error::other)?;
                    }
                    Event::SessionOpened { session } => client.track_session(app, session.id, true),
                    Event::SessionClosed { session_id, .. } => {
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
struct SessionOutputPayload {
    session_id: SessionId,
    data: String,
}

#[derive(Debug, Clone, Serialize)]
struct DisconnectedPayload {
    reason: String,
}

#[tauri::command]
fn connect_daemon(app: AppHandle, state: State<'_, HitchClient>) -> Result<(), String> {
    state.connect(&app)?;
    match state.send_request(
        &app,
        Request::Hello {
            client_name: "hitch-desktop".into(),
            protocol_version: PROTOCOL_VERSION,
        },
    )? {
        Response::Hello { .. } => Ok(()),
        Response::Error { error } => Err(error.message),
        other => Err(format!("unexpected hello response: {other:?}")),
    }
}

#[tauri::command]
fn hitch_request(
    app: AppHandle,
    state: State<'_, HitchClient>,
    request: Request,
) -> Result<Response, String> {
    state.send_request(&app, request)
}

#[tauri::command]
fn send_session_input(
    app: AppHandle,
    state: State<'_, HitchClient>,
    session_id: SessionId,
    data: String,
) -> Result<Response, String> {
    let bytes = data.into_bytes();
    state.send_request_with_payload(
        &app,
        Request::SendSessionInput {
            session_id,
            byte_count: bytes.len() as u32,
        },
        Some(bytes),
    )
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

    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .menu(&menu)
        .tooltip(tray_status_text(false, 0))
        .on_menu_event(handle_tray_menu_event);
    if let Some(icon) = app.default_window_icon().cloned() {
        builder = builder.icon(icon);
    }
    builder.build(app)?;

    if let Ok(mut slot) = app.state::<HitchClient>().0.tray_status.lock() {
        *slot = Some(status);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::tray_status_text;

    #[test]
    fn tray_status_text_reflects_connection_and_count() {
        assert_eq!(tray_status_text(false, 0), "Hitch — daemon offline");
        assert_eq!(tray_status_text(false, 3), "Hitch — daemon offline");
        assert_eq!(tray_status_text(true, 0), "Hitch — no active sessions");
        assert_eq!(tray_status_text(true, 1), "Hitch — running 1 session");
        assert_eq!(tray_status_text(true, 4), "Hitch — running 4 sessions");
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(HitchClient::new())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
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
            send_session_input
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
