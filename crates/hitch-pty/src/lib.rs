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

use hitch_core::{SessionId, SESSION_ID_ENV};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
#[cfg(windows)]
use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE},
    System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    },
};

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
    /// Program + args. `None` means `$SHELL -l`/`/bin/sh -l` on Unix or `cmd.exe` on Windows.
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
    #[cfg(windows)]
    job: WindowsJobObject,
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
        let child = pair.slave.spawn_command(build_command(
            &config.session_id,
            &config.command,
            &config.cwd,
        ))?;
        #[cfg(windows)]
        let (child, job) = {
            let mut child = child;
            let job = match WindowsJobObject::for_child(child.as_ref()) {
                Ok(job) => job,
                Err(err) => {
                    let _ = child.kill();
                    return Err(err);
                }
            };
            (child, job)
        };
        drop(pair.slave);

        let pty = Arc::new(Self {
            session_id: config.session_id,
            master: Mutex::new(pair.master),
            writer: Mutex::new(writer),
            child: Mutex::new(child),
            #[cfg(windows)]
            job,
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

    /// Force the child to repaint: re-apply the PTY's current size, then send
    /// SIGWINCH to the child's process group UNCONDITIONALLY. A same-size
    /// TIOCSWINSZ emits no SIGWINCH, so the explicit signal is what makes a
    /// full-screen app (e.g. Claude Code) re-emit a correctly-sized frame; a
    /// shell at its prompt simply redraws in place, so the signal is always safe.
    pub fn repaint(&self) -> Result<(), PtyError> {
        let master = self
            .master
            .lock()
            .map_err(|_| PtyError::Poisoned("master"))?;
        // Re-apply the current size to recover from any grid drift. Best-effort:
        // if reading the size fails we skip this and still signal below.
        if let Ok(size) = master.get_size() {
            let _ = master.resize(size);
        }
        #[cfg(unix)]
        {
            let leader = master.process_group_leader();
            if let Some(pgid) = leader {
                // SAFETY: `kill(2)` with a negative pid signals the process group;
                // it has no memory-safety preconditions and an already-gone group
                // returns ESRCH, which we ignore. SIGWINCH is harmless to a shell.
                unsafe {
                    libc::kill(-pgid, libc::SIGWINCH);
                }
            }
        }
        Ok(())
    }

    /// Kill the child process.
    pub fn kill(&self) -> Result<(), PtyError> {
        let mut child = self.child.lock().map_err(|_| PtyError::Poisoned("child"))?;
        #[cfg(windows)]
        {
            let job_result = self.job.terminate();
            let _ = child.kill();
            job_result
        }
        #[cfg(not(windows))]
        {
            child.kill()?;
            Ok(())
        }
    }

    /// Best-effort name of the PTY's foreground process group leader — the
    /// command the user is currently interacting with (a tool launched inside
    /// the shell, or the shell itself). `None` when it can't be resolved.
    #[cfg(unix)]
    pub fn foreground_command(&self) -> Option<String> {
        let pid = {
            let master = self.master.lock().ok()?;
            master.process_group_leader()?
        };
        command_name_for_pid(pid)
    }

    /// Return a best-effort foreground command when the PTY backend exposes one.
    #[cfg(not(unix))]
    pub fn foreground_command(&self) -> Option<String> {
        None
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

#[cfg(windows)]
struct WindowsJobObject {
    handle: HANDLE,
}

#[cfg(windows)]
unsafe impl Send for WindowsJobObject {}

#[cfg(windows)]
unsafe impl Sync for WindowsJobObject {}

#[cfg(windows)]
impl WindowsJobObject {
    fn for_child(child: &(dyn Child + Send + Sync)) -> Result<Self, PtyError> {
        let process = child.as_raw_handle().ok_or_else(|| {
            PtyError::Io(io::Error::new(
                io::ErrorKind::Unsupported,
                "PTY child does not expose a Windows process handle",
            ))
        })?;

        // SAFETY: Passing null security attributes and name requests a new unnamed
        // job object. On failure Windows returns a null handle, converted below.
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle.is_null() {
            return Err(PtyError::Io(io::Error::last_os_error()));
        }

        let job = Self { handle };
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;

        // SAFETY: `job.handle` is a live job handle owned by `job`, `limits` points
        // to a properly initialized JOBOBJECT_EXTENDED_LIMIT_INFORMATION, and the
        // byte count matches that structure.
        let configured = unsafe {
            SetInformationJobObject(
                job.handle,
                JobObjectExtendedLimitInformation,
                std::ptr::addr_of!(limits).cast(),
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if configured == 0 {
            return Err(PtyError::Io(io::Error::last_os_error()));
        }

        // SAFETY: `process` is the process handle exposed by portable-pty for the
        // spawned child, and `job.handle` is configured to kill members on close.
        let assigned = unsafe { AssignProcessToJobObject(job.handle, process.cast()) };
        if assigned == 0 {
            return Err(PtyError::Io(io::Error::last_os_error()));
        }

        Ok(job)
    }

    fn terminate(&self) -> Result<(), PtyError> {
        // SAFETY: `self.handle` is a live job handle for this PTY until Drop.
        if unsafe { TerminateJobObject(self.handle, 1) } == 0 {
            return Err(PtyError::Io(io::Error::last_os_error()));
        }
        Ok(())
    }
}

#[cfg(windows)]
impl Drop for WindowsJobObject {
    fn drop(&mut self) {
        // SAFETY: `handle` is owned by this wrapper and closed exactly once here.
        unsafe {
            CloseHandle(self.handle);
        }
    }
}

fn build_command(
    session_id: &SessionId,
    command: &Option<Vec<String>>,
    cwd: &Path,
) -> CommandBuilder {
    let mut builder = if let Some(command) = command {
        let mut builder = CommandBuilder::new(&command[0]);
        if command.len() > 1 {
            builder.args(&command[1..]);
        }
        builder
    } else {
        default_command()
    };
    builder.env("TERM", "xterm-256color");
    builder.env(SESSION_ID_ENV, session_id.to_string());
    builder.cwd(cwd);
    builder
}

#[cfg(windows)]
fn default_command() -> CommandBuilder {
    CommandBuilder::new(std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_string()))
}

#[cfg(unix)]
fn default_command() -> CommandBuilder {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
    let mut builder = CommandBuilder::new(shell);
    builder.arg("-l");
    builder
}

#[cfg(not(any(unix, windows)))]
fn default_command() -> CommandBuilder {
    CommandBuilder::new("sh")
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
    let executable = std::path::Path::new(path).file_name()?.to_string_lossy();
    normalize_command_name(&executable, command_line_args_for_pid(pid).as_deref())
}

#[cfg(target_os = "linux")]
fn command_name_for_pid(pid: libc::pid_t) -> Option<String> {
    let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
    let name = comm.trim();
    if name.is_empty() {
        return None;
    }
    let args = command_line_args_for_pid(pid);
    normalize_command_name(name, args.as_deref())
}

#[cfg_attr(not(unix), allow(dead_code))]
fn normalize_command_name(executable: &str, args: Option<&[String]>) -> Option<String> {
    runtime_agent_command(executable, args)
        .map(str::to_string)
        .or_else(|| Some(executable.to_string()))
}

#[cfg_attr(not(unix), allow(dead_code))]
fn runtime_agent_command(executable: &str, args: Option<&[String]>) -> Option<&'static str> {
    if !executable.eq_ignore_ascii_case("node") {
        return None;
    }

    for arg in args? {
        if path_basename_or_stem_eq(arg, "codex") {
            return Some("codex");
        }
        if path_basename_or_stem_eq(arg, "claude") || path_has_component(arg, "claude-code") {
            return Some("claude");
        }
    }

    None
}

#[cfg_attr(not(unix), allow(dead_code))]
fn path_basename_or_stem_eq(path: &str, expected: &str) -> bool {
    let Some(name) = std::path::Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
    else {
        return false;
    };
    name.eq_ignore_ascii_case(expected)
        || std::path::Path::new(name)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .is_some_and(|stem| stem.eq_ignore_ascii_case(expected))
}

/// True when any path component of `arg` equals `expected` (case-insensitively).
/// Recognizes an agent CLI by its package/bin directory (e.g.
/// `.../@anthropic-ai/claude-code/cli.js`) without matching unrelated scripts
/// that merely embed the name as a substring (e.g. `./scripts/claude-codegen.js`).
#[cfg_attr(not(unix), allow(dead_code))]
fn path_has_component(arg: &str, expected: &str) -> bool {
    std::path::Path::new(arg).components().any(|component| {
        component
            .as_os_str()
            .to_str()
            .is_some_and(|part| part.eq_ignore_ascii_case(expected))
    })
}

#[cfg(target_os = "macos")]
fn command_line_args_for_pid(pid: libc::pid_t) -> Option<Vec<String>> {
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
    let mut size = 0_usize;
    if unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as u32,
            std::ptr::null_mut(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    } != 0
    {
        return None;
    }

    let mut buf = vec![0_u8; size];
    if unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as u32,
            buf.as_mut_ptr() as *mut libc::c_void,
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    } != 0
    {
        return None;
    }
    buf.truncate(size);
    parse_macos_procargs(&buf)
}

#[cfg(target_os = "macos")]
fn parse_macos_procargs(buf: &[u8]) -> Option<Vec<String>> {
    let argc = i32::from_ne_bytes(buf.get(..std::mem::size_of::<i32>())?.try_into().ok()?);
    if argc <= 0 {
        return None;
    }

    let mut idx = std::mem::size_of::<i32>();
    while idx < buf.len() && buf[idx] != 0 {
        idx += 1;
    }
    while idx < buf.len() && buf[idx] == 0 {
        idx += 1;
    }

    parse_nul_args(&buf[idx..], argc as usize)
}

#[cfg(target_os = "linux")]
fn command_line_args_for_pid(pid: libc::pid_t) -> Option<Vec<String>> {
    parse_nul_args(
        &std::fs::read(format!("/proc/{pid}/cmdline")).ok()?,
        usize::MAX,
    )
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn parse_nul_args(buf: &[u8], limit: usize) -> Option<Vec<String>> {
    let args = buf
        .split(|byte| *byte == 0)
        .filter(|arg| !arg.is_empty())
        .take(limit)
        .map(|arg| String::from_utf8_lossy(arg).into_owned())
        .collect::<Vec<_>>();
    (!args.is_empty()).then_some(args)
}
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
#[allow(dead_code)]
fn command_name_for_pid<P>(_pid: P) -> Option<String> {
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
    #[cfg(unix)]
    use std::time::{Duration, Instant};

    #[test]
    fn scrollback_keeps_latest_bytes() {
        let buffer = ScrollbackBuffer::new(5);
        buffer.append(b"abc");
        buffer.append(b"def");
        assert_eq!(buffer.snapshot(), b"bcdef");
    }

    #[test]
    fn node_runtime_commands_report_agent_cli_names() {
        assert_eq!(
            normalize_command_name(
                "node",
                Some(&[
                    "node".into(),
                    "/opt/homebrew/lib/node_modules/@openai/codex/bin/codex.js".into(),
                ]),
            ),
            Some("codex".into()),
        );
        assert_eq!(
            normalize_command_name(
                "node",
                Some(&[
                    "node".into(),
                    "/usr/local/lib/node_modules/@anthropic-ai/claude-code/cli.js".into(),
                ]),
            ),
            Some("claude".into()),
        );
    }

    #[test]
    fn ordinary_node_commands_still_report_node() {
        assert_eq!(
            normalize_command_name("node", Some(&["node".into(), "server.js".into()])),
            Some("node".into()),
        );
    }

    #[test]
    fn node_scripts_embedding_agent_names_still_report_node() {
        // A script whose name merely contains "claude-code" as a substring must
        // not be misreported as the Claude CLI.
        assert_eq!(
            normalize_command_name(
                "node",
                Some(&["node".into(), "./scripts/claude-codegen.js".into()]),
            ),
            Some("node".into()),
        );
    }

    #[cfg(unix)]
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

    #[cfg(unix)]
    #[test]
    fn spawned_pty_exports_hitch_session_id() {
        let (tx, rx) = mpsc::channel();
        let session_id = SessionId::new();
        let pty = ManagedPty::spawn(
            PtySpawnConfig::new(session_id, std::env::current_dir().unwrap())
                .command(Some(vec![
                    "/bin/sh".into(),
                    "-lc".into(),
                    format!("printf %s \"${SESSION_ID_ENV}\""),
                ]))
                .scrollback_capacity(1024),
            tx,
        )
        .unwrap();

        let output = collect_output(&rx, Duration::from_secs(3));
        assert!(String::from_utf8_lossy(&output).contains(&session_id.to_string()));
        assert!(String::from_utf8_lossy(&pty.scrollback()).contains(&session_id.to_string()));
    }

    #[cfg(unix)]
    #[test]
    fn repaint_resolves_leader_and_is_idempotent() {
        // Spawn a long-lived shell so the PTY has a live process group leader,
        // then repaint twice. Both calls exercise size re-apply + the SIGWINCH
        // signal path and must return Ok, proving the path runs and is
        // idempotent (a same-size resize plus an unconditional SIGWINCH is safe
        // to repeat).
        let (tx, _rx) = mpsc::channel();
        let session_id = SessionId::new();
        let pty = ManagedPty::spawn(
            PtySpawnConfig::new(session_id, std::env::current_dir().unwrap())
                .command(Some(vec!["/bin/sh".into(), "-c".into(), "sleep 5".into()]))
                .scrollback_capacity(1024),
            tx,
        )
        .unwrap();

        assert!(pty.repaint().is_ok());
        assert!(pty.repaint().is_ok());

        let _ = pty.kill();
    }

    #[cfg(unix)]
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
