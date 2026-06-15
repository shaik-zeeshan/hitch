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
        Name as LocalSocketName, RecvHalf, SendHalf,
    },
    os::windows::{local_socket::ListenerOptionsExt, security_descriptor::SecurityDescriptor},
    TryClone as _,
};
#[cfg(windows)]
use std::os::windows::io::{AsHandle, AsRawHandle};

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

/// Per-user data directory for the current build namespace. This is the single
/// canonical resolver: the daemon (store, managed worktrees, log) and the GUI
/// (log tail) both call it so they can never drift onto different roots.
///
/// Unix keeps the historical `$HOME/.hitch*` layout, falling back through
/// `HOME` → `USERPROFILE` → the system temp dir so a process with a stripped
/// environment still lands somewhere writable. Windows uses
/// `%LOCALAPPDATA%\Hitch`, with a namespace child for dev or custom instances
/// so store, logs, and pipe rendezvous stay isolated.
pub fn default_data_dir() -> PathBuf {
    #[cfg(unix)]
    {
        unix_home_dir().join(instance_dir_name())
    }
    #[cfg(windows)]
    {
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
}

/// Resolve the user's home directory on Unix, falling back HOME → USERPROFILE →
/// temp. Kept here so the data-root layout is owned by one crate; the daemon and
/// GUI must not re-derive it independently (they drifted when they did).
#[cfg(unix)]
fn unix_home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
        .unwrap_or_else(std::env::temp_dir)
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
            // Restrict the named pipe to its creating user — the named-pipe
            // equivalent of a 0700 socket (ADR 0012). Without this the pipe is
            // created with the default DACL, which is permissive enough for
            // other local users to attach. See `owner_only_security_descriptor`.
            .security_descriptor(owner_only_security_descriptor()?)
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

    /// The real OS address another process must use to reach this listener,
    /// suitable to hand a child as `SSH_AUTH_SOCK`. Unlike [`local_path`], which
    /// returns the stored *logical* path, this is always a connectable endpoint
    /// string: the socket file path on Unix, or `\\.\pipe\hitch-<hash>` on
    /// Windows (where the bound endpoint is a named pipe, not the logical path).
    pub fn os_address(&self) -> String {
        endpoint_os_address(&self.path)
    }
}

/// Real OS address string for an endpoint bound at `path`, suitable to hand a
/// child process as `SSH_AUTH_SOCK` (the ssh-agent relay's per-connection
/// socket, ADR 0014 amendment).
///
/// On Unix the bound endpoint *is* a filesystem socket at `path` (that is what
/// [`UnixListener::bind`] binds), so the address is the path string itself.
#[cfg(unix)]
pub fn endpoint_os_address(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// Real OS address string for an endpoint bound at `path` (see the Unix variant).
///
/// On Windows the bound endpoint is a named pipe whose name is derived from the
/// *logical* path by [`logical_socket_name`]; interprocess's `GenericNamespaced`
/// namespace prefixes that name with `\\.\pipe\` at bind/connect time, so the
/// connectable address is `\\.\pipe\hitch-<hash>`. Returning the logical path (as
/// [`local_path`] does) would not be connectable. The hash stays in one place
/// ([`logical_socket_name`]) so this is byte-for-byte what `bind` created.
#[cfg(windows)]
pub fn endpoint_os_address(path: &Path) -> String {
    format!(r"\\.\pipe\{}", logical_socket_name(path))
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
///
/// A successful connect obviously means "accepting". On Windows a connect can
/// also fail with `ERROR_PIPE_BUSY` when every pipe instance is momentarily
/// occupied — in the narrow window while the daemon's accept thread re-arms a
/// fresh instance after the previous accept (ADR 0012), or against a stale daemon
/// that still polls. Either way that is a *live* daemon, not an absent one, so it
/// counts as "accepting" too. Any other connect error (NotFound / refused) means
/// nothing is listening.
pub fn endpoint_accepts_connections(path: &Path) -> bool {
    match connect_daemon(path) {
        Ok(_) => true,
        Err(err) => is_endpoint_busy(&err),
    }
}

/// True when a connect error means the endpoint exists and is bound but every
/// pipe instance is momentarily busy (`ERROR_PIPE_BUSY`, 231). This is a
/// transient "all instances occupied" state on a live Windows named-pipe server —
/// the daemon's accept thread re-arming between connections, or a stale polling
/// daemon between polls (ADR 0012) — distinct from a NotFound/refused error that
/// means nothing is listening. Always false off Windows, where a Unix socket has
/// no equivalent busy state.
pub fn is_endpoint_busy(err: &io::Error) -> bool {
    #[cfg(windows)]
    {
        // ERROR_PIPE_BUSY: all pipe instances are busy.
        err.raw_os_error() == Some(231)
    }
    #[cfg(not(windows))]
    {
        let _ = err;
        false
    }
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
        // ADR 0012 names `GetNamedPipeServerProcessId` for server-pid discovery.
        // interprocess's `peer_creds().pid()` is the safe wrapper over exactly
        // that Win32 call (it dispatches to `GetNamedPipeServerProcessId` for a
        // client-side pipe handle), so this is the ADR's primitive by another
        // name — no manual handle juggling needed.
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

    /// Split this connected stream into a relay reader + writer pair (ADR 0014
    /// amendment, ssh-agent relay). Both halves address the SAME underlying
    /// connection: on Unix they are clones of one `UnixStream`; on Windows they are
    /// the receive/send halves of one named-pipe stream, which share a single duplex
    /// pipe handle (interprocess's `split` ref-clones it). Either half's
    /// `force_close` therefore unblocks a read pending on the other. The control/PTY
    /// decoders are discarded — a relay channel is a raw byte bridge.
    pub fn into_relay_channel(self) -> io::Result<(RelayReader, RelayWriter)> {
        #[cfg(unix)]
        {
            let writer = self.stream.try_clone()?;
            // Bound the inbound GUI->git write (the control-reader thread does a
            // blocking `write_all` to this half): a wedged git child that stops
            // draining its socket could otherwise stall that thread — and thus the
            // whole connection's control plane — forever. A timeout turns that into
            // a recoverable `WouldBlock`/`TimedOut` the daemon treats as "close this
            // channel". Best-effort: if the platform rejects the option, fall back to
            // the prior unbounded behavior rather than failing the channel.
            let _ = writer.set_write_timeout(Some(RELAY_WRITE_TIMEOUT));
            Ok((RelayReader { stream: self.stream }, RelayWriter { stream: writer }))
        }
        #[cfg(windows)]
        {
            let (recv, send) = self.stream.split();
            Ok((RelayReader { half: recv }, RelayWriter { half: send }))
        }
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

/// Read half of an ssh-agent relay channel (ADR 0014 amendment). The daemon's
/// pump thread reads the git child's request bytes (git → daemon → GUI) from it.
#[derive(Debug)]
pub struct RelayReader {
    #[cfg(unix)]
    stream: UnixStream,
    #[cfg(windows)]
    half: RecvHalf,
}

impl RelayReader {
    /// Unblock a thread blocked in [`Read::read`] on this reader. Unix:
    /// `shutdown(Both)`. Windows: `CancelIoEx` + `DisconnectNamedPipe` on the pipe
    /// handle the pending `ReadFile` is using. Idempotent / best-effort.
    pub fn force_close(&self) -> io::Result<()> {
        #[cfg(unix)]
        { shutdown_both(&self.stream) }
        #[cfg(windows)]
        { force_close_recv_half(&self.half) }
    }
}

impl Read for RelayReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        #[cfg(unix)]
        { self.stream.read(buf) }
        #[cfg(windows)]
        { let mut r = &self.half; r.read(buf) }
    }
}

/// How long the inbound GUI→git write half may block before the daemon gives up
/// on a wedged git child and closes the channel (ADR 0014 amendment). Generous
/// enough to never trip on a healthy child (an ssh-agent reply is a few hundred
/// bytes), short enough that one stuck child cannot stall the control plane
/// indefinitely. Unix only — Windows named pipes do not support write timeouts.
#[cfg(unix)]
const RELAY_WRITE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Write half of an ssh-agent relay channel (ADR 0014 amendment). Inbound GUI
/// reply bytes (GUI → daemon → git) are written to it. Held behind an `Arc` by the
/// daemon and written through a shared `&RelayWriter`, so it impls `Write` for both
/// `RelayWriter` and `&RelayWriter`.
#[derive(Debug)]
pub struct RelayWriter {
    #[cfg(unix)]
    stream: UnixStream,
    #[cfg(windows)]
    half: SendHalf,
}

impl RelayWriter {
    /// Unblock a read pending on the paired [`RelayReader`] and signal teardown to
    /// the git child. Unix: `shutdown(Both)` (peer reads EOF). Windows: `CancelIoEx`
    /// + `DisconnectNamedPipe` on the shared pipe handle (same handle the reader's
    /// `ReadFile` uses); the disconnect breaks the pipe so the git child sees the
    /// connection end immediately, not only once both halves drop. Idempotent /
    /// best-effort.
    pub fn force_close(&self) -> io::Result<()> {
        #[cfg(unix)]
        { shutdown_both(&self.stream) }
        #[cfg(windows)]
        { force_close_send_half(&self.half) }
    }
}

impl Write for RelayWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        #[cfg(unix)]
        { self.stream.write(buf) }
        #[cfg(windows)]
        { let mut w = &self.half; w.write(buf) }
    }
    fn flush(&mut self) -> io::Result<()> {
        #[cfg(unix)]
        { self.stream.flush() }
        #[cfg(windows)]
        { Ok(()) }
    }
}

/// Write through a shared reference so the daemon can store the writer in an `Arc`
/// and relay inbound chunks without cloning or holding the channel-map lock across
/// the write.
impl Write for &RelayWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        #[cfg(unix)]
        { let mut s = &self.stream; s.write(buf) }
        #[cfg(windows)]
        { let mut w = &self.half; w.write(buf) }
    }
    fn flush(&mut self) -> io::Result<()> {
        #[cfg(unix)]
        { let mut s = &self.stream; s.flush() }
        #[cfg(windows)]
        { Ok(()) }
    }
}

#[cfg(unix)]
fn shutdown_both(stream: &UnixStream) -> io::Result<()> {
    match stream.shutdown(std::net::Shutdown::Both) {
        Ok(()) => Ok(()),
        // Already shut down / peer gone — idempotent teardown is not an error.
        Err(err) if err.kind() == io::ErrorKind::NotConnected => Ok(()),
        Err(err) => Err(err),
    }
}

#[cfg(windows)]
fn force_close_recv_half(half: &RecvHalf) -> io::Result<()> {
    match half {
        RecvHalf::NamedPipe(np) => force_close_pipe_handle(np.as_handle().as_raw_handle()),
    }
}

#[cfg(windows)]
fn force_close_send_half(half: &SendHalf) -> io::Result<()> {
    match half {
        SendHalf::NamedPipe(np) => force_close_pipe_handle(np.as_handle().as_raw_handle()),
    }
}

/// Make a relay channel's named-pipe handle behave like a Unix `shutdown(Both)`:
/// unblock any thread parked in `ReadFile` right now AND break the connection so
/// the NEXT read also fails and the peer (git) sees the pipe end immediately —
/// not only once both shared-handle halves finally drop. `CancelIoEx` only
/// cancels I/O already pending in the kernel (not sticky), so on its own it misses
/// the window where the relay pump is between reads; `DisconnectNamedPipe` is the
/// sticky half and the true `shutdown(Both)` analog. Both are best-effort:
/// `ERROR_NOT_FOUND` (nothing pending) and a disconnect on an already-broken or
/// non-server handle are benign, so neither is treated as an error.
#[cfg(windows)]
fn force_close_pipe_handle(handle: std::os::windows::io::RawHandle) -> io::Result<()> {
    // SAFETY: `handle` is borrowed from a half we still hold, so it is a live pipe
    // handle for the duration of these calls, which only act on it. A null
    // OVERLAPPED cancels every pending operation on the handle.
    unsafe {
        let _ = windows_sys::Win32::System::IO::CancelIoEx(handle as _, std::ptr::null());
        let _ = windows_sys::Win32::System::Pipes::DisconnectNamedPipe(handle as _);
    }
    Ok(())
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

/// SDDL for the daemon pipe's DACL: a *protected* (`P`, inheritance-blocking)
/// DACL whose single ACE grants `GenericAll` (`GA`) to the object's creator
/// owner (`OW`) — the user that bound the listener.
///
/// A non-null DACL with no ACE for a principal denies that principal entirely,
/// so this is allow-owner / deny-everyone-else: the named-pipe equivalent of a
/// `0700` Unix socket promised by ADR 0012. We deliberately do *not* add ACEs
/// for SYSTEM or Administrators: the daemon and all its clients (GUI, hook) run
/// as the same interactive user, so owner-only is both sufficient and the
/// tightest grant. We use `OW` rather than the bound user's literal SID so the
/// descriptor is a fixed string with no runtime SID lookup; Windows resolves
/// `OW` to the creating process's owner when the pipe instance is created.
#[cfg(windows)]
const DAEMON_PIPE_SDDL: &str = "D:P(A;;GA;;;OW)";

/// Build the owner-restricted security descriptor applied to the daemon pipe.
///
/// Deserializes [`DAEMON_PIPE_SDDL`] via interprocess's
/// `SecurityDescriptor::deserialize`, which wraps `ConvertStringSecurityDescriptorToSecurityDescriptorW`.
#[cfg(windows)]
fn owner_only_security_descriptor() -> io::Result<SecurityDescriptor> {
    let sddl = widestring::U16CString::from_str(DAEMON_PIPE_SDDL)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err))?;
    SecurityDescriptor::deserialize(&sddl)
}

#[cfg(windows)]
fn windows_socket_name(path: &Path) -> io::Result<LocalSocketName<'static>> {
    logical_socket_name(path).to_ns_name::<GenericNamespaced>()
}

#[cfg(windows)]
fn logical_socket_name(path: &Path) -> String {
    // Windows paths are case-insensitive and accept both separators, so the same
    // endpoint can reach this helper spelled several ways: `C:\foo`, `c:\foo`, and
    // `C:/foo` are one path but hash to three different pipe names. The daemon, the
    // GUI, and the hook each derive their socket path independently (default,
    // `--socket`, or `HITCH_SOCKET`), so a differently-spelled override would land
    // them on mismatched pipes. Normalize the spelling — separators to `\`,
    // everything lowercased — before hashing so equivalent paths rendezvous.
    let path = normalize_socket_spelling(path);
    // FNV-1a over the normalized spelling's bytes (the shared leaf-crate hash, so
    // pipe names stay byte-for-byte stable across crates and builds).
    let hash = hitch_core::fnv1a_64(path.as_bytes());
    format!("hitch-{hash:016x}")
}

/// Normalize the *spelling* of a Windows path so case- and separator-equivalent
/// paths produce identical bytes. This is a pure string transform: it does not
/// touch the filesystem, resolve symlinks, or canonicalize `.`/`..`, so it stays
/// correct for sockets that do not exist yet and never blocks on I/O.
#[cfg(windows)]
fn normalize_socket_spelling(path: &Path) -> String {
    path.as_os_str()
        .to_string_lossy()
        .chars()
        .map(|ch| if ch == '/' { '\\' } else { ch })
        .collect::<String>()
        .to_lowercase()
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
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

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
                    os_family: crate::OsFamily::Unix,
                    exe_path: None,
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
                    os_family: crate::OsFamily::Unix,
                    exe_path: None,
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

    #[cfg(windows)]
    #[test]
    fn owner_only_security_descriptor_builds_and_listener_accepts_same_user_client() {
        // The owner-restricted SDDL must deserialize into a valid descriptor...
        owner_only_security_descriptor()
            .expect("owner-only security descriptor should deserialize from SDDL");

        // ...and a listener bound with it must still accept a connection from the
        // same user (the only principal granted GenericAll). This guards against
        // an over-tight descriptor that would lock the daemon out of its own pipe.
        let path = test_socket_path();
        let listener = DaemonListener::bind(&path).unwrap();
        let server = thread::spawn(move || {
            let _conn = listener.accept().unwrap();
        });
        let _client = DaemonClient::connect(&path).expect("same-user client should attach");
        server.join().unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn busy_endpoint_is_not_mistaken_for_a_released_one() {
        // ERROR_PIPE_BUSY (231) is a live, momentarily-saturated pipe server, so
        // `is_endpoint_busy` must recognize it; a NotFound is a genuinely absent
        // endpoint and must not.
        assert!(is_endpoint_busy(&io::Error::from_raw_os_error(231)));
        assert!(!is_endpoint_busy(&io::Error::from(io::ErrorKind::NotFound)));
    }

    #[cfg(windows)]
    #[test]
    fn logical_socket_name_is_stable_across_equivalent_spellings() {
        // Case- and separator-equivalent spellings of one Windows path must hash
        // to the same pipe name, or a daemon and a client that derived their
        // socket path from differently-spelled overrides would never rendezvous.
        let canonical =
            logical_socket_name(Path::new(r"C:\Users\pc\AppData\Local\Hitch\daemon.sock"));

        for variant in [
            r"c:\users\pc\appdata\local\hitch\daemon.sock", // lowercased drive + path
            r"C:/Users/pc/AppData/Local/Hitch/daemon.sock", // forward slashes
            r"C:\Users\PC\AppData\Local\Hitch\Daemon.sock", // mixed case
        ] {
            assert_eq!(
                logical_socket_name(Path::new(variant)),
                canonical,
                "spelling {variant:?} should hash to the same pipe name",
            );
        }

        // Genuinely different paths must still produce different names.
        assert_ne!(
            logical_socket_name(Path::new(r"C:\Users\pc\AppData\Local\Hitch\daemon.sock")),
            logical_socket_name(Path::new(r"C:\Users\pc\AppData\Local\Hitch\other.sock")),
        );
    }

    #[cfg(unix)]
    #[test]
    fn endpoint_os_address_is_the_socket_path_on_unix() {
        // On Unix the bound endpoint IS the socket file, so the address handed to
        // a child as SSH_AUTH_SOCK is the path string verbatim.
        let path = Path::new("/tmp/hitch/agent/7.sock");
        assert_eq!(endpoint_os_address(path), "/tmp/hitch/agent/7.sock");
    }

    #[cfg(windows)]
    #[test]
    fn endpoint_os_address_is_the_named_pipe_on_windows() {
        // On Windows the bound endpoint is a named pipe; the address must be the
        // connectable `\\.\pipe\hitch-<hash>`, matching what `bind` created via
        // `logical_socket_name`, and stable across equivalent path spellings.
        let canonical = endpoint_os_address(Path::new(
            r"C:\Users\pc\AppData\Local\Hitch\agent\7.sock",
        ));
        assert!(
            canonical.starts_with(r"\\.\pipe\hitch-"),
            "address {canonical:?} should be a hitch named pipe",
        );
        assert_eq!(
            canonical,
            format!(
                r"\\.\pipe\{}",
                logical_socket_name(Path::new(r"C:\Users\pc\AppData\Local\Hitch\agent\7.sock"))
            ),
            "address must reuse the same hash logical_socket_name/bind use",
        );
        for variant in [
            r"c:\users\pc\appdata\local\hitch\agent\7.sock",
            r"C:/Users/pc/AppData/Local/Hitch/agent/7.sock",
        ] {
            assert_eq!(
                endpoint_os_address(Path::new(variant)),
                canonical,
                "spelling {variant:?} should resolve to the same pipe address",
            );
        }
        // A genuinely different endpoint must produce a different address.
        assert_ne!(
            endpoint_os_address(Path::new(r"C:\Users\pc\AppData\Local\Hitch\agent\8.sock")),
            canonical,
        );
    }

    #[test]
    fn relay_channel_round_trips_and_force_close_unblocks_reader() {
        let path = test_socket_path();
        let listener = DaemonListener::bind(&path).unwrap();
        let accept = thread::spawn(move || {
            let stream = listener.accept().unwrap();
            stream.into_relay_channel().unwrap()
            // `listener` drops here; the accepted connection persists via the halves.
        });

        let mut client = connect_daemon(&path).unwrap();
        let (mut reader, writer) = accept.join().unwrap();

        // git -> daemon: bytes arrive at the reader verbatim.
        client.write_all(&[1, 2, 3, 4, 5]).unwrap();
        client.flush().unwrap();
        let mut got = [0u8; 5];
        reader.read_exact(&mut got).unwrap();
        assert_eq!(got, [1, 2, 3, 4, 5]);

        // daemon -> git: writer bytes reach the client verbatim (write through &writer,
        // the path the daemon uses).
        {
            let mut w = &writer;
            w.write_all(&[9, 8, 7]).unwrap();
            w.flush().unwrap();
        }
        let mut back = [0u8; 3];
        client.read_exact(&mut back).unwrap();
        assert_eq!(back, [9, 8, 7]);

        // A reader blocked on a pending read unblocks promptly after force_close.
        let (tx, rx) = std::sync::mpsc::channel();
        let blocked = thread::spawn(move || {
            let mut buf = [0u8; 16];
            let r = reader.read(&mut buf); // blocks: no more bytes are coming
            let _ = tx.send(());
            r
        });
        thread::sleep(Duration::from_millis(150));
        writer.force_close().unwrap();
        rx.recv_timeout(Duration::from_secs(5))
            .expect("reader must unblock shortly after force_close");
        match blocked.join().unwrap() {
            // Unix shutdown -> clean EOF.
            Ok(0) => {}
            Ok(n) => panic!("unexpected {n} bytes after force_close"),
            // Windows cancel / broken pipe is an acceptable "EOF" too.
            Err(_) => {}
        }

        drop(client);
        #[cfg(unix)]
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn default_data_dir_ends_with_instance_dir_name() {
        // The shared resolver the daemon and GUI both call must land both of them
        // on the same per-instance root. On Unix the leaf is the historical
        // `.hitch*` dir name; on Windows it is the `Hitch` (plus namespace) tree.
        let dir = default_data_dir();
        #[cfg(unix)]
        {
            let leaf = dir.file_name().unwrap().to_string_lossy().into_owned();
            assert_eq!(leaf, instance_dir_name());
        }
        #[cfg(windows)]
        {
            assert!(
                dir.to_string_lossy().contains("Hitch"),
                "windows data dir {dir:?} should live under a Hitch tree",
            );
        }
    }

    fn test_socket_path() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("hitch-proto-test-{nonce}.sock"))
    }
}
