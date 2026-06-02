//! Minimal platform-neutral daemon transport scaffolding.
//!
//! This module intentionally provides blocking, low-level primitives. Higher
//! level connection lifecycle, subscriptions, retries, and async task ownership
//! belong in `hitch-daemon` / `src-tauri`, not in the protocol crate.

#[cfg(unix)]
use std::fs;
use std::io::{self, Read, Write};
#[cfg(unix)]
use std::os::unix::fs::FileTypeExt;
#[cfg(unix)]
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};

#[cfg(windows)]
use interprocess::{
    local_socket::{
        prelude::*, GenericNamespaced, ListenerNonblockingMode, ListenerOptions,
        Name as LocalSocketName,
    },
    TryClone as _,
};

use crate::framing::{
    encode_control_message, encode_pty_frame, ControlLineDecoder, FrameError, PtyFrameDecoder,
};
use crate::message::ControlMessage;

/// Isolation namespace for this build, so a debug build and an installed
/// release build run fully independent daemons (separate socket, store,
/// worktrees, and log) and can coexist without touching each other's sessions.
///
/// Keyed on the build profile: release builds get the empty namespace, debug
/// builds get `dev`. Because `cargo tauri dev` and `cargo run -p hitch-daemon`
/// are both debug while the bundled app and its `--release` sidecar daemon are
/// both release, every Hitch process self-selects the matching namespace with
/// no flags to thread through spawn. `HITCH_INSTANCE` overrides it (an empty
/// value forces the release namespace).
pub fn instance_namespace() -> String {
    if let Some(value) = std::env::var_os("HITCH_INSTANCE") {
        return value.to_string_lossy().trim().to_string();
    }
    if cfg!(debug_assertions) {
        "dev".to_string()
    } else {
        String::new()
    }
}

/// The namespace rendered as a path infix: `""` for release, `"-dev"` for debug.
fn instance_infix() -> String {
    let namespace = instance_namespace();
    if namespace.is_empty() {
        String::new()
    } else {
        format!("-{namespace}")
    }
}

/// Name of the per-instance data directory under `$HOME` (`.hitch` for release,
/// `.hitch-dev` for debug). Holds the store, managed worktrees, and daemon log.
pub fn instance_dir_name() -> String {
    format!(".hitch{}", instance_infix())
}

/// Per-user data directory for Windows builds. Release uses `%LOCALAPPDATA%\Hitch`;
/// debug/custom instances live under a namespace child so store, logs, and pipe
/// rendezvous stay isolated without drifting across crates.
#[cfg(windows)]
pub fn default_data_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("Hitch");
    let namespace = instance_namespace();
    if namespace.is_empty() {
        base
    } else {
        base.join(namespace)
    }
}

/// Default daemon socket path for the current user and build namespace.
#[cfg(unix)]
pub fn default_socket_path() -> PathBuf {
    std::env::temp_dir().join(format!("hitch{}-{}.sock", instance_infix(), current_uid()))
}

/// Default daemon endpoint for the current user and build namespace.
#[cfg(windows)]
pub fn default_socket_path() -> PathBuf {
    default_data_dir().join("daemon.sock")
}

/// Path to the daemon's pidfile, derived from its socket path.
///
/// The daemon writes its pid here when it binds the socket and removes it on a
/// clean shutdown. This is the one place the daemon's pid is discoverable
/// *without* a successful `Hello` handshake — so a client that connects to a
/// protocol-incompatible daemon (which never returns a pid) can still SIGKILL it
/// and respawn a compatible one. Both sides derive the path from the socket path
/// through this single helper so they can never drift onto different files.
pub fn pidfile_path(socket_path: &Path) -> PathBuf {
    socket_path.with_extension("pid")
}

/// Blocking client connection to the daemon endpoint.
#[derive(Debug)]
pub struct DaemonClient {
    connection: DaemonStream,
}

impl DaemonClient {
    pub fn connect(path: impl AsRef<Path>) -> io::Result<Self> {
        Ok(Self {
            connection: connect_daemon(path)?,
        })
    }

    pub fn into_connection(self) -> DaemonStream {
        self.connection
    }

    pub fn connection_mut(&mut self) -> &mut DaemonStream {
        &mut self.connection
    }
}

/// Backwards-compatible Unix-named client surface.
pub type UnixSocketClient = DaemonClient;

/// Backwards-compatible Unix-named daemon-side listener surface.
pub type UnixSocketListener = DaemonListener;

/// Backwards-compatible Unix-named connected stream surface.
pub type UnixSocketConnection = DaemonStream;

#[cfg(unix)]
type PlatformListener = UnixListener;

#[cfg(windows)]
type PlatformListener = LocalSocketListener;

#[cfg(unix)]
type PlatformStream = UnixStream;

#[cfg(windows)]
type PlatformStream = LocalSocketStream;

/// Blocking daemon-side listener.
#[derive(Debug)]
pub struct DaemonListener {
    listener: PlatformListener,
    path: PathBuf,
}

impl DaemonListener {
    /// Bind a socket path. On Unix, if a stale filesystem socket exists at that
    /// path, it is removed first. On Windows, the path is a logical per-user
    /// endpoint name mapped to an Interprocess local socket backed by named pipes.
    pub fn bind(path: impl AsRef<Path>) -> io::Result<Self> {
        let path = path.as_ref().to_path_buf();
        #[cfg(unix)]
        let listener = {
            remove_stale_socket(&path)?;
            UnixListener::bind(&path)?
        };
        #[cfg(windows)]
        let listener = ListenerOptions::new()
            .name(windows_socket_name(&path)?)
            .create_sync()?;
        Ok(Self { listener, path })
    }

    /// Accept one client connection.
    pub fn accept(&self) -> io::Result<DaemonStream> {
        #[cfg(unix)]
        {
            let (stream, _) = self.listener.accept()?;
            Ok(DaemonStream::from_stream(stream))
        }
        #[cfg(windows)]
        {
            Ok(DaemonStream::from_stream(self.listener.accept()?))
        }
    }

    pub fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        #[cfg(unix)]
        {
            self.listener.set_nonblocking(nonblocking)
        }
        #[cfg(windows)]
        {
            self.listener
                .set_nonblocking(ListenerNonblockingMode::from_bool(nonblocking, false))
        }
    }

    pub fn local_path(&self) -> &Path {
        &self.path
    }
}

impl Drop for DaemonListener {
    fn drop(&mut self) {
        #[cfg(unix)]
        let _ = fs::remove_file(&self.path);
    }
}

/// Connect to the daemon endpoint at `path`.
pub fn connect_daemon(path: impl AsRef<Path>) -> io::Result<DaemonStream> {
    let path = path.as_ref();
    #[cfg(unix)]
    {
        DaemonStream::connect(path)
    }
    #[cfg(windows)]
    {
        DaemonStream::connect(path)
    }
}

/// Return whether a daemon endpoint currently accepts connections.
pub fn endpoint_accepts_connections(path: &Path) -> bool {
    connect_daemon(path).is_ok()
}

/// A single connected daemon socket with incremental control and PTY decoders.
#[derive(Debug)]
pub struct DaemonStream {
    stream: PlatformStream,
    control_decoder: ControlLineDecoder,
    pty_decoder: PtyFrameDecoder,
}

impl DaemonStream {
    #[cfg(unix)]
    pub fn new(stream: UnixStream) -> Self {
        Self::from_stream(stream)
    }

    fn connect(path: &Path) -> io::Result<Self> {
        #[cfg(unix)]
        {
            Ok(Self::from_stream(UnixStream::connect(path)?))
        }
        #[cfg(windows)]
        {
            Ok(Self::from_stream(LocalSocketStream::connect(
                windows_socket_name(path)?,
            )?))
        }
    }

    fn from_stream(stream: PlatformStream) -> Self {
        Self {
            stream,
            control_decoder: ControlLineDecoder::new(),
            pty_decoder: PtyFrameDecoder::new(),
        }
    }

    pub fn try_clone(&self) -> io::Result<Self> {
        Ok(Self::from_stream(self.stream.try_clone()?))
    }

    pub fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        self.stream.set_nonblocking(nonblocking)
    }

    pub fn send_control(&mut self, message: &ControlMessage) -> Result<(), TransportError> {
        let bytes = encode_control_message(message)?;
        self.stream.write_all(&bytes)?;
        Ok(())
    }

    pub fn send_pty_frame(&mut self, payload: &[u8]) -> Result<(), TransportError> {
        let bytes = encode_pty_frame(payload)?;
        self.stream.write_all(&bytes)?;
        Ok(())
    }

    /// Read from the socket and decode control messages from the bytes read.
    ///
    /// Use this on a control-plane connection. PTY frame reads are exposed via
    /// [`Self::read_pty_frames`], allowing later daemon code to keep data planes
    /// on separate streams or route by prior control messages.
    pub fn read_control_messages(&mut self) -> Result<Vec<ControlMessage>, TransportError> {
        let mut buf = [0_u8; 8192];
        let len = self.stream.read(&mut buf)?;
        if len == 0 {
            return Err(TransportError::ConnectionClosed);
        }
        Ok(self.control_decoder.push(&buf[..len])?)
    }

    /// Read from the socket and decode PTY frames from the bytes read.
    pub fn read_pty_frames(&mut self) -> Result<Vec<Vec<u8>>, TransportError> {
        let mut buf = [0_u8; 8192];
        let len = self.stream.read(&mut buf)?;
        if len == 0 {
            return Err(TransportError::ConnectionClosed);
        }
        Ok(self.pty_decoder.push(&buf[..len])?)
    }

    #[cfg(windows)]
    pub fn connected_pipe_server_pid(&self) -> io::Result<u32> {
        self.stream.peer_creds()?.pid().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::Unsupported,
                "connected pipe peer did not expose a process id",
            )
        })
    }

    #[cfg(unix)]
    pub fn into_inner(self) -> UnixStream {
        self.stream
    }
}

impl Read for DaemonStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.stream.read(buf)
    }
}

impl Write for DaemonStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.stream.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}

/// Transport-level error, wrapping I/O and framing failures.
#[derive(Debug)]
pub enum TransportError {
    Io(io::Error),
    Frame(FrameError),
    ConnectionClosed,
}

impl std::fmt::Display for TransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(err) => std::fmt::Display::fmt(err, f),
            Self::Frame(err) => std::fmt::Display::fmt(err, f),
            Self::ConnectionClosed => write!(f, "socket connection closed"),
        }
    }
}

impl std::error::Error for TransportError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            Self::Frame(err) => Some(err),
            Self::ConnectionClosed => None,
        }
    }
}

impl From<io::Error> for TransportError {
    fn from(err: io::Error) -> Self {
        Self::Io(err)
    }
}

impl From<FrameError> for TransportError {
    fn from(err: FrameError) -> Self {
        Self::Frame(err)
    }
}

#[cfg(windows)]
fn windows_socket_name(path: &Path) -> io::Result<LocalSocketName<'static>> {
    logical_socket_name(path).to_ns_name::<GenericNamespaced>()
}

#[cfg(windows)]
fn logical_socket_name(path: &Path) -> String {
    const FNV_OFFSET: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x00000100000001b3;

    let path = path.as_os_str().to_string_lossy();
    let mut hash = FNV_OFFSET;
    for byte in path.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    format!("hitch-{hash:016x}")
}

#[cfg(unix)]
fn remove_stale_socket(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_socket() => fs::remove_file(path),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("{} exists and is not a socket", path.display()),
        )),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

#[cfg(unix)]
fn current_uid() -> u32 {
    // Avoid adding a libc dependency to the protocol crate for a diagnostic path.
    std::env::var("UID")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{ControlMessage, Request, Response, PROTOCOL_VERSION};
    use std::thread;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn client_and_listener_exchange_control_message() {
        let path = test_socket_path();
        let listener = DaemonListener::bind(&path).unwrap();
        let server = thread::spawn(move || {
            let mut conn = listener.accept().unwrap();
            let messages = conn.read_control_messages().unwrap();
            assert_eq!(messages.len(), 1);
            conn.send_control(&ControlMessage::response(
                1,
                Response::Hello {
                    protocol_version: PROTOCOL_VERSION,
                    daemon_pid: 42,
                },
            ))
            .unwrap();
        });

        let mut client = DaemonClient::connect(&path).unwrap();
        client
            .connection_mut()
            .send_control(&ControlMessage::request(
                1,
                Request::Hello {
                    client_name: "test".into(),
                    protocol_version: PROTOCOL_VERSION,
                },
            ))
            .unwrap();
        let response = client.connection_mut().read_control_messages().unwrap();
        assert_eq!(
            response,
            vec![ControlMessage::response(
                1,
                Response::Hello {
                    protocol_version: PROTOCOL_VERSION,
                    daemon_pid: 42,
                }
            )]
        );

        server.join().unwrap();
        #[cfg(unix)]
        let _ = fs::remove_file(path);
    }

    #[cfg(windows)]
    #[test]
    fn connected_pipe_server_pid_identifies_listener_process() {
        let path = test_socket_path();
        let listener = DaemonListener::bind(&path).unwrap();
        let (accepted_tx, accepted_rx) = std::sync::mpsc::channel();
        let server = thread::spawn(move || {
            let _conn = listener.accept().unwrap();
            accepted_tx.send(()).unwrap();
            thread::sleep(std::time::Duration::from_millis(100));
        });

        let client = DaemonClient::connect(&path).unwrap();
        accepted_rx.recv().unwrap();
        let server_pid = client.connection.connected_pipe_server_pid().unwrap();
        assert_eq!(server_pid, std::process::id());

        drop(client);
        server.join().unwrap();
    }

    fn test_socket_path() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("hitch-proto-test-{nonce}.sock"))
    }
}
