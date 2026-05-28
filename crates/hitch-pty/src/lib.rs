//! `hitch-pty` — PTY ownership for Hitch (ADR 0003).
//!
//! This crate owns the OS-facing PTY primitive used by the daemon spike:
//! spawn a child process inside a pseudo-terminal, stream output into a bounded
//! scrollback buffer, accept input/resize requests, and terminate the child on
//! demand. It intentionally depends only on `hitch-core` plus PTY plumbing.

use std::collections::VecDeque;
use std::fmt;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;

use hitch_core::SessionId;
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};

const DEFAULT_COLS: u16 = 120;
const DEFAULT_ROWS: u16 = 40;
/// Default capacity of a session's bounded scrollback ring. Exported so the
/// daemon's dispatcher can bound its own authoritative broadcast log to the same
/// size as the reader ring it mirrors — the two must hold the same window of
/// bytes for replay to reproduce what the ring would have returned.
pub const DEFAULT_SCROLLBACK_CAPACITY: usize = 1024 * 1024;
/// Reader thread read buffer size. A larger buffer means fewer output frames
/// and less chance of splitting multi-byte sequences across reads under heavy
/// output. This 64 KB array lives on the reader thread's stack.
const READER_BUFFER_BYTES: usize = 64 * 1024;

/// Size of a terminal in character cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalSize {
    pub cols: u16,
    pub rows: u16,
}

impl TerminalSize {
    pub fn new(cols: u16, rows: u16) -> Self {
        Self { cols, rows }
    }
}

impl Default for TerminalSize {
    fn default() -> Self {
        Self {
            cols: DEFAULT_COLS,
            rows: DEFAULT_ROWS,
        }
    }
}

impl From<TerminalSize> for PtySize {
    fn from(size: TerminalSize) -> Self {
        Self {
            rows: size.rows,
            cols: size.cols,
            pixel_width: 0,
            pixel_height: 0,
        }
    }
}

/// Configuration for spawning a PTY process.
#[derive(Debug, Clone)]
pub struct PtySpawnConfig {
    pub session_id: SessionId,
    pub cwd: PathBuf,
    /// Program + args. `None` means `$SHELL -l` or `/bin/sh -l`.
    pub command: Option<Vec<String>>,
    pub size: TerminalSize,
    pub scrollback_capacity: usize,
}

impl PtySpawnConfig {
    pub fn new(session_id: SessionId, cwd: impl Into<PathBuf>) -> Self {
        Self {
            session_id,
            cwd: cwd.into(),
            command: None,
            size: TerminalSize::default(),
            scrollback_capacity: DEFAULT_SCROLLBACK_CAPACITY,
        }
    }

    pub fn command(mut self, command: Option<Vec<String>>) -> Self {
        self.command = command;
        self
    }

    pub fn size(mut self, size: TerminalSize) -> Self {
        self.size = size;
        self
    }

    pub fn scrollback_capacity(mut self, capacity: usize) -> Self {
        self.scrollback_capacity = capacity;
        self
    }
}

/// Output/exit notifications emitted by a managed PTY.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PtyEvent {
    Output {
        session_id: SessionId,
        bytes: Vec<u8>,
    },
    Exited {
        session_id: SessionId,
        exit_code: Option<i32>,
    },
}

/// A live PTY process with bounded scrollback.
pub struct ManagedPty {
    session_id: SessionId,
    master: Mutex<Box<dyn MasterPty + Send>>,
    writer: Mutex<Box<dyn Write + Send>>,
    child: Mutex<Box<dyn Child + Send + Sync>>,
    scrollback: ScrollbackBuffer,
}

impl ManagedPty {
    /// Spawn a PTY and start a reader thread that fills scrollback and sends
    /// [`PtyEvent`] notifications.
    pub fn spawn(
        config: PtySpawnConfig,
        events: mpsc::Sender<PtyEvent>,
    ) -> Result<Arc<Self>, PtyError> {
        if config
            .command
            .as_ref()
            .is_some_and(|command| command.is_empty())
        {
            return Err(PtyError::EmptyCommand);
        }
        if !config.cwd.is_dir() {
            return Err(PtyError::InvalidCwd(config.cwd));
        }

        let pty_system = native_pty_system();
        let pair = pty_system.openpty(config.size.into())?;
        let reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;
        let child = pair
            .slave
            .spawn_command(build_command(&config.command, &config.cwd))?;
        drop(pair.slave);

        let pty = Arc::new(Self {
            session_id: config.session_id,
            master: Mutex::new(pair.master),
            writer: Mutex::new(writer),
            child: Mutex::new(child),
            scrollback: ScrollbackBuffer::new(config.scrollback_capacity),
        });

        spawn_reader_thread(Arc::clone(&pty), reader, events);
        Ok(pty)
    }

    pub fn session_id(&self) -> SessionId {
        self.session_id
    }

    /// Write bytes to the child process.
    pub fn write_input(&self, bytes: &[u8]) -> Result<(), PtyError> {
        let mut writer = self
            .writer
            .lock()
            .map_err(|_| PtyError::Poisoned("writer"))?;
        writer.write_all(bytes)?;
        writer.flush()?;
        Ok(())
    }

    /// Resize the terminal.
    pub fn resize(&self, size: TerminalSize) -> Result<(), PtyError> {
        let master = self
            .master
            .lock()
            .map_err(|_| PtyError::Poisoned("master"))?;
        master.resize(size.into())?;
        Ok(())
    }

    /// Kill the child process.
    pub fn kill(&self) -> Result<(), PtyError> {
        let mut child = self.child.lock().map_err(|_| PtyError::Poisoned("child"))?;
        child.kill()?;
        Ok(())
    }

    /// Best-effort name of the PTY's foreground process group leader — the
    /// command the user is currently interacting with (a tool launched inside
    /// the shell, or the shell itself). `None` when it can't be resolved.
    pub fn foreground_command(&self) -> Option<String> {
        let pid = {
            let master = self.master.lock().ok()?;
            master.process_group_leader()?
        };
        command_name_for_pid(pid)
    }

    /// Return a point-in-time copy of buffered scrollback bytes.
    pub fn scrollback(&self) -> Vec<u8> {
        self.scrollback.snapshot()
    }

    fn append_scrollback(&self, bytes: &[u8]) {
        self.scrollback.append(bytes);
    }

    fn wait_for_exit_code(&self) -> Option<i32> {
        let mut child = self.child.lock().ok()?;
        child
            .wait()
            .ok()
            .map(|status| i32::try_from(status.exit_code()).unwrap_or(i32::MAX))
    }
}

fn spawn_reader_thread(
    pty: Arc<ManagedPty>,
    mut reader: Box<dyn Read + Send>,
    events: mpsc::Sender<PtyEvent>,
) {
    thread::Builder::new()
        .name(format!("hitch-pty-reader-{}", pty.session_id()))
        .spawn(move || {
            let mut buf = [0_u8; READER_BUFFER_BYTES];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(len) => {
                        let bytes = buf[..len].to_vec();
                        pty.append_scrollback(&bytes);
                        if events
                            .send(PtyEvent::Output {
                                session_id: pty.session_id(),
                                bytes,
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }

            let exit_code = pty.wait_for_exit_code();
            let _ = events.send(PtyEvent::Exited {
                session_id: pty.session_id(),
                exit_code,
            });
        })
        .expect("failed to spawn PTY reader thread");
}

fn build_command(command: &Option<Vec<String>>, cwd: &Path) -> CommandBuilder {
    let mut builder = if let Some(command) = command {
        let mut builder = CommandBuilder::new(&command[0]);
        if command.len() > 1 {
            builder.args(&command[1..]);
        }
        builder
    } else {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
        let mut builder = CommandBuilder::new(shell);
        builder.arg("-l");
        builder
    };
    builder.env("TERM", "xterm-256color");
    builder.cwd(cwd);
    builder
}

#[cfg(target_os = "macos")]
fn command_name_for_pid(pid: libc::pid_t) -> Option<String> {
    let mut buf = [0_u8; 4096];
    let ret =
        unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr() as *mut libc::c_void, buf.len() as u32) };
    if ret <= 0 {
        return None;
    }
    let path = std::str::from_utf8(&buf[..ret as usize]).ok()?;
    std::path::Path::new(path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
}

#[cfg(target_os = "linux")]
fn command_name_for_pid(pid: libc::pid_t) -> Option<String> {
    let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
    let name = comm.trim();
    (!name.is_empty()).then(|| name.to_string())
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn command_name_for_pid(_pid: libc::pid_t) -> Option<String> {
    None
}

#[derive(Debug)]
pub enum PtyError {
    Io(io::Error),
    Pty(anyhow::Error),
    EmptyCommand,
    InvalidCwd(PathBuf),
    Poisoned(&'static str),
}

impl fmt::Display for PtyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => fmt::Display::fmt(err, f),
            Self::Pty(err) => fmt::Display::fmt(err, f),
            Self::EmptyCommand => write!(f, "PTY command must include a program"),
            Self::InvalidCwd(path) => write!(f, "PTY cwd is not a directory: {}", path.display()),
            Self::Poisoned(lock) => write!(f, "PTY {lock} lock was poisoned"),
        }
    }
}

impl std::error::Error for PtyError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            Self::Pty(err) => Some(err.as_ref()),
            Self::EmptyCommand | Self::InvalidCwd(_) | Self::Poisoned(_) => None,
        }
    }
}

impl From<io::Error> for PtyError {
    fn from(err: io::Error) -> Self {
        Self::Io(err)
    }
}

impl From<anyhow::Error> for PtyError {
    fn from(err: anyhow::Error) -> Self {
        Self::Pty(err)
    }
}

struct ScrollbackBuffer {
    capacity: usize,
    bytes: Mutex<VecDeque<u8>>,
}

impl ScrollbackBuffer {
    fn new(capacity: usize) -> Self {
        Self {
            capacity,
            bytes: Mutex::new(VecDeque::with_capacity(capacity.min(8192))),
        }
    }

    fn append(&self, chunk: &[u8]) {
        if self.capacity == 0 || chunk.is_empty() {
            return;
        }

        let mut bytes = match self.bytes.lock() {
            Ok(bytes) => bytes,
            Err(_) => return,
        };

        if chunk.len() >= self.capacity {
            bytes.clear();
            bytes.extend(chunk[chunk.len() - self.capacity..].iter().copied());
            return;
        }

        let overflow = bytes
            .len()
            .saturating_add(chunk.len())
            .saturating_sub(self.capacity);
        for _ in 0..overflow {
            bytes.pop_front();
        }
        bytes.extend(chunk.iter().copied());
    }

    fn snapshot(&self) -> Vec<u8> {
        self.bytes
            .lock()
            .map(|bytes| bytes.iter().copied().collect())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn scrollback_keeps_latest_bytes() {
        let buffer = ScrollbackBuffer::new(5);
        buffer.append(b"abc");
        buffer.append(b"def");
        assert_eq!(buffer.snapshot(), b"bcdef");
    }

    #[test]
    fn spawned_pty_streams_output_into_scrollback() {
        let (tx, rx) = mpsc::channel();
        let session_id = SessionId::new();
        let pty = ManagedPty::spawn(
            PtySpawnConfig::new(session_id, std::env::current_dir().unwrap())
                .command(Some(vec![
                    "/bin/sh".into(),
                    "-lc".into(),
                    "printf hitch-pty-test".into(),
                ]))
                .scrollback_capacity(1024),
            tx,
        )
        .unwrap();

        let output = collect_output(&rx, Duration::from_secs(3));
        assert!(String::from_utf8_lossy(&output).contains("hitch-pty-test"));
        assert!(String::from_utf8_lossy(&pty.scrollback()).contains("hitch-pty-test"));
    }

    fn collect_output(rx: &mpsc::Receiver<PtyEvent>, timeout: Duration) -> Vec<u8> {
        let deadline = Instant::now() + timeout;
        let mut output = Vec::new();
        while Instant::now() < deadline {
            match rx.recv_timeout(Duration::from_millis(50)) {
                Ok(PtyEvent::Output { bytes, .. }) => output.extend(bytes),
                Ok(PtyEvent::Exited { .. }) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        output
    }
}
