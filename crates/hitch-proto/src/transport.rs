//! Minimal Unix-domain socket transport scaffolding.
//!
//! This module intentionally provides blocking, low-level primitives. Higher
//! level connection lifecycle, subscriptions, retries, and async task ownership
//! belong in `hitch-daemon` / `src-tauri`, not in the protocol crate.

use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::fs::FileTypeExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};

use crate::framing::{
    encode_control_message, encode_pty_frame, ControlLineDecoder, FrameError, PtyFrameDecoder,
};
use crate::message::ControlMessage;

/// Default daemon socket path for the current user.
pub fn default_socket_path() -> PathBuf {
    std::env::temp_dir().join(format!("hitch-{}.sock", current_uid()))
}

/// Blocking client connection to the daemon socket.
#[derive(Debug)]
pub struct UnixSocketClient {
    connection: UnixSocketConnection,
}

impl UnixSocketClient {
    pub fn connect(path: impl AsRef<Path>) -> io::Result<Self> {
        Ok(Self {
            connection: UnixSocketConnection::new(UnixStream::connect(path)?),
        })
    }

    pub fn into_connection(self) -> UnixSocketConnection {
        self.connection
    }

    pub fn connection_mut(&mut self) -> &mut UnixSocketConnection {
        &mut self.connection
    }
}

/// Blocking daemon-side listener.
#[derive(Debug)]
pub struct UnixSocketListener {
    listener: UnixListener,
    path: PathBuf,
}

impl UnixSocketListener {
    /// Bind a socket path. If a stale filesystem socket exists at that path, it
    /// is removed first.
    pub fn bind(path: impl AsRef<Path>) -> io::Result<Self> {
        let path = path.as_ref().to_path_buf();
        remove_stale_socket(&path)?;
        let listener = UnixListener::bind(&path)?;
        Ok(Self { listener, path })
    }

    /// Accept one client connection.
    pub fn accept(&self) -> io::Result<UnixSocketConnection> {
        let (stream, _) = self.listener.accept()?;
        Ok(UnixSocketConnection::new(stream))
    }

    pub fn local_path(&self) -> &Path {
        &self.path
    }
}

impl Drop for UnixSocketListener {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// A single connected Unix socket with incremental control and PTY decoders.
#[derive(Debug)]
pub struct UnixSocketConnection {
    stream: UnixStream,
    control_decoder: ControlLineDecoder,
    pty_decoder: PtyFrameDecoder,
}

impl UnixSocketConnection {
    pub fn new(stream: UnixStream) -> Self {
        Self {
            stream,
            control_decoder: ControlLineDecoder::new(),
            pty_decoder: PtyFrameDecoder::new(),
        }
    }

    pub fn try_clone(&self) -> io::Result<Self> {
        Ok(Self::new(self.stream.try_clone()?))
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

    pub fn into_inner(self) -> UnixStream {
        self.stream
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
        let listener = UnixSocketListener::bind(&path).unwrap();
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

        let mut client = UnixSocketClient::connect(&path).unwrap();
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
        let _ = fs::remove_file(path);
    }

    fn test_socket_path() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("hitch-proto-test-{nonce}.sock"))
    }
}
