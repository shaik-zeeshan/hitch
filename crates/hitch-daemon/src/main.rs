//! `hitch-daemon` — the long-lived process that owns every PTY, buffers
//! scrollback, persists layout, and serves the `hitch-proto` socket (ADR 0003).
//!
//! Slice 7 daemon composition: this binary is the sole composer of the feature
//! crates (ADR 0005). It wires store + git + PTY + agent-hook installation into
//! the socket API consumed by the desktop client and `hitch-hook`.

use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::OsString;
#[cfg(unix)]
use std::fs::OpenOptions;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Read, Write};
#[cfg(unix)]
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

mod drafts;

/// How long a session's PTY may stay quiet before the output-activity gate falls
/// to inactive (ADR 0011 amendment 2026-06-05). Agent TUIs repaint spinners
/// continuously, so genuine in-progress work holds the gate open; an interrupted
/// or hung agent stops emitting and the gate drops, taking the downstream
/// `WORKING` word with it. Tunable: PLAN.md slice 7 calibrates this against real
/// agent TUIs at implementation-verification time (~3–5s; 4s to start).
const OUTPUT_ACTIVE_QUIET: Duration = Duration::from_secs(4);

/// How often the output-activity poller scans for sessions that have gone quiet
/// past [`OUTPUT_ACTIVE_QUIET`] and fires their falling edge. Mirrors the other
/// per-second daemon pollers.
const OUTPUT_ACTIVE_POLL_INTERVAL: Duration = Duration::from_millis(1000);

use drafts::{CommitDraftInput, DraftProviderConfig, PullRequestDraftInput};
use hitch_agent::HookInstallOptions;
use hitch_core::{
    AgentState, JobId, Project, ProjectId, ProjectKind, Session, SessionId, SessionParent,
    Worktree, WorktreeId,
};
use hitch_git::{
    staged_diff, CommandControl, CreatePrRequest, CreateWorktreeRequest, DiffTarget, FileState,
    GitClient, GitRepository, StatusEntry, WorktreeCheckout,
};
use hitch_process::{DrainOutcome, PipeReader, ProcessTree};
use hitch_proto::{
    encode_control_message, encode_pty_frame,
    transport::{connect_daemon, DaemonListener, DaemonStream},
    ChangedFile, CommitDraft, ControlMessage, DraftGenerationSettings, DraftProvider, ErrorCode,
    Event, FileDiff, FileStatus, GitStatus, JobRequest, JobStatus, KnownAgent, PrInfo,
    ProtocolError, PullRequestDraft, Request, Response, WorktreeCreateMode, WorktreePr,
    MAX_PTY_FRAME_LEN, PROTOCOL_VERSION,
};
use hitch_pty::{ManagedPty, PtyEvent, PtySpawnConfig, TerminalSize, DEFAULT_SCROLLBACK_CAPACITY};
use hitch_store::Store;

fn main() {
    if let Err(err) = real_main() {
        eprintln!("hitch-daemon: {err}");
        std::process::exit(1);
    }
}

fn real_main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse()?;
    if args.detach {
        detach_spawn(&args)?;
        return Ok(());
    }

    run_daemon(DaemonConfig::from(args))?;
    Ok(())
}

/// Resolve the user's login-shell PATH. On macOS, GUI apps (and their
/// descendants) inherit a stripped PATH from launchd; running the login
/// shell recovers the full user PATH set up by shell profile files.
fn login_shell_path() -> Option<String> {
    let shell = std::env::var("SHELL").ok().filter(|s| !s.is_empty())?;
    let output = Command::new(&shell)
        .args(["-l", "-c", "printenv PATH"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let path = String::from_utf8(output.stdout).ok()?;
    let path = path.trim().to_string();
    if path.is_empty() {
        None
    } else {
        Some(path)
    }
}

#[derive(Debug)]
struct Args {
    socket_path: PathBuf,
    store_path: PathBuf,
    managed_root: PathBuf,
    hook_helper: PathBuf,
    git: PathBuf,
    gh: PathBuf,
    draft_provider: DraftProviderConfig,
    detach: bool,
}

impl Args {
    fn parse() -> Result<Self, String> {
        let mut socket_path = hitch_proto::transport::default_socket_path();
        let mut store_path = default_store_path();
        let mut managed_root = default_managed_worktree_root();
        let mut hook_helper = default_hook_helper_path();
        let mut git = PathBuf::from("git");
        let mut gh = PathBuf::from("gh");
        let mut draft_provider = DraftProviderConfig::from_env()?;
        let mut detach = false;
        let mut args = std::env::args().skip(1);

        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--socket" => {
                    socket_path = PathBuf::from(
                        args.next()
                            .ok_or_else(|| "--socket requires a path".to_string())?,
                    );
                }
                "--store" => {
                    store_path = PathBuf::from(
                        args.next()
                            .ok_or_else(|| "--store requires a path".to_string())?,
                    );
                }
                "--managed-root" => {
                    managed_root = PathBuf::from(
                        args.next()
                            .ok_or_else(|| "--managed-root requires a path".to_string())?,
                    );
                }
                "--hook-helper" => {
                    hook_helper = PathBuf::from(
                        args.next()
                            .ok_or_else(|| "--hook-helper requires a path".to_string())?,
                    );
                }
                "--git" => {
                    git = PathBuf::from(
                        args.next()
                            .ok_or_else(|| "--git requires a path".to_string())?,
                    );
                }
                "--gh" => {
                    gh = PathBuf::from(
                        args.next()
                            .ok_or_else(|| "--gh requires a path".to_string())?,
                    );
                }
                "--draft-provider" => {
                    let value = args.next().ok_or_else(|| {
                        "--draft-provider requires stub, claude, or codex".to_string()
                    })?;
                    draft_provider.set_kind(&value)?;
                }
                "--claude" => {
                    draft_provider.claude = PathBuf::from(
                        args.next()
                            .ok_or_else(|| "--claude requires a path".to_string())?,
                    );
                }
                "--codex" => {
                    draft_provider.codex = PathBuf::from(
                        args.next()
                            .ok_or_else(|| "--codex requires a path".to_string())?,
                    );
                }
                "--draft-timeout-secs" => {
                    let value = args
                        .next()
                        .ok_or_else(|| "--draft-timeout-secs requires a value".to_string())?;
                    draft_provider.set_timeout_secs(&value)?;
                }
                "--draft-model" => {
                    let model = args
                        .next()
                        .ok_or_else(|| "--draft-model requires a value".to_string())?
                        .trim()
                        .to_string();
                    draft_provider.model = (!model.is_empty()).then_some(model);
                }
                "--detach" => detach = true,
                "--help" | "-h" => {
                    println!(
                        "usage: hitch-daemon [--socket PATH] [--store PATH] [--managed-root PATH] [--hook-helper PATH] [--git PATH] [--gh PATH] [--draft-provider stub|claude|codex] [--draft-model MODEL] [--claude PATH] [--codex PATH] [--draft-timeout-secs N] [--detach]"
                    );
                    std::process::exit(0);
                }
                unknown => return Err(format!("unknown argument: {unknown}")),
            }
        }

        Ok(Self {
            socket_path,
            store_path,
            managed_root,
            hook_helper,
            git,
            gh,
            draft_provider,
            detach,
        })
    }
}

#[derive(Debug, Clone)]
struct DaemonConfig {
    socket_path: PathBuf,
    store_path: PathBuf,
    managed_root: PathBuf,
    hook_helper: PathBuf,
    git: PathBuf,
    gh: PathBuf,
    draft_provider: DraftProviderConfig,
}

impl From<Args> for DaemonConfig {
    fn from(args: Args) -> Self {
        Self {
            socket_path: args.socket_path,
            store_path: args.store_path,
            managed_root: args.managed_root,
            hook_helper: args.hook_helper,
            git: args.git,
            gh: args.gh,
            draft_provider: args.draft_provider,
        }
    }
}

fn detach_spawn(args: &Args) -> io::Result<()> {
    let exe = std::env::current_exe()?;
    let mut child = Command::new(exe);
    // Keep the detached daemon's console invisible no matter who launched this
    // `--detach` shim. The GUI client already spawns the shim windowless, but a
    // stale or manually launched shim could carry a visible console — and the
    // daemon would inherit it and pin the window open for its whole lifetime.
    // Giving the daemon its own hidden console here also keeps its console
    // children (git, draft providers) windowless.
    hitch_process::configure_windowless(&mut child);
    child
        .arg("--socket")
        .arg(&args.socket_path)
        .arg("--store")
        .arg(&args.store_path)
        .arg("--managed-root")
        .arg(&args.managed_root)
        .arg("--hook-helper")
        .arg(&args.hook_helper)
        .arg("--git")
        .arg(&args.git)
        .arg("--gh")
        .arg(&args.gh)
        .arg("--draft-provider")
        .arg(args.draft_provider.kind.label())
        .arg("--claude")
        .arg(&args.draft_provider.claude)
        .arg("--codex")
        .arg(&args.draft_provider.codex)
        .arg("--draft-timeout-secs")
        .arg(args.draft_provider.timeout.as_secs().to_string());
    if let Some(model) = args.draft_provider.model.as_deref() {
        child.arg("--draft-model").arg(model);
    }
    // Resolve the login-shell PATH now (in the short-lived --detach process)
    // and bake it into the daemon's environment. The detached daemon inherits
    // launchd's stripped PATH otherwise, making claude/codex unfindable.
    if let Some(path) = login_shell_path() {
        child.env("PATH", path);
    }
    // Redirect the detached daemon's stdout+stderr to a rotated log file beside
    // the socket/store instead of /dev/null (ADR 0009). This is the one change
    // that captures the `eprintln!` fatal path, Rust's default panic output, the
    // panic hook below, and library noise — so a startup crash (socket bind,
    // store open, panic) leaves a reason the GUI can tail, rather than an opaque
    // timeout. Rotation happens here (once per spawn = "on start"); the file
    // handles are inherited by the real daemon, which owns them after we exit.
    let log = rotate_and_open_log(&daemon_log_path())?;
    let log_err = log.try_clone()?;
    let child = child
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err))
        .spawn()?;
    println!("{}", child.id());
    Ok(())
}

/// Path to the daemon's log, beside the socket and store. Computed from the same
/// [`data_dir`] the store/managed-root defaults use so the Tauri client's
/// `read_daemon_log_tail` and this writer never drift onto different files
/// (ADR 0009).
fn daemon_log_path() -> PathBuf {
    data_dir().join("daemon.log")
}

/// Rotate `daemon.log` → `daemon.log.prev` and open a fresh log for writing.
///
/// Rotate-on-start (not size-based) keeps exactly two files: the current run and
/// the immediately preceding one. A daemon that crashes and is respawned by the
/// GUI thus preserves the crash trace in `.prev` while the new run writes a fresh
/// `daemon.log` — so the failure reason survives its own respawn.
fn rotate_and_open_log(path: &Path) -> io::Result<fs::File> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    // Rotate only when a previous log exists; a missing file is the first run.
    if path.exists() {
        let prev = path.with_extension("log.prev");
        // A stale `.prev` is overwritten — we keep only the last two runs.
        let _ = fs::rename(path, &prev);
    }
    fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)
}

/// Install a panic hook that flushes a located panic line to stderr — which the
/// detached daemon has pointed at `daemon.log` (see `detach_spawn`). Without this
/// a panicking worker thread (PTY reader, poller, Job worker) would still print
/// Rust's default message, but the explicit hook guarantees the location and an
/// explicit flush so the reason reaches the log before the process unwinds.
fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let location = info
            .location()
            .map(|loc| format!("{}:{}:{}", loc.file(), loc.line(), loc.column()))
            .unwrap_or_else(|| "unknown location".to_string());
        let message = panic_message(info.payload());
        eprintln!("hitch-daemon: panic at {location}: {message}");
        let _ = io::stderr().flush();
        // Preserve Rust's default behavior (backtrace handling, abort-on-panic).
        default_hook(info);
    }));
}

/// Best-effort extraction of a panic payload's message for the log line.
fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "non-string panic payload".to_string()
    }
}

fn run_daemon(config: DaemonConfig) -> io::Result<()> {
    install_panic_hook();

    if let Some(parent) = config.socket_path.parent() {
        fs::create_dir_all(parent)?;
    }
    if let Some(parent) = config.store_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::create_dir_all(&config.managed_root)?;

    let listener = DaemonListener::bind(&config.socket_path)?;
    // The listener stays in blocking mode: a dedicated accept thread parks in
    // `accept()` so no poll gap exists in which a connect-write-close client is
    // dropped (see the accept thread below and ADR 0012). The old nonblocking
    // poll loop is gone, not cfg-switched.
    #[cfg(unix)]
    let pid_lock = {
        // Record our pid beside the socket so the GUI can force-kill us even when it
        // never completed a `Hello` handshake (e.g. a protocol mismatch — the path
        // that returns no pid in the response). Best-effort: a missing pidfile only
        // costs the client its force-kill fast path, it does not break startup.
        match write_pidfile(&config.socket_path) {
            Ok(pid_lock) => pid_lock,
            Err(err) => {
                eprintln!("hitch-daemon: pidfile recovery disabled: {err}");
                None
            }
        }
    };
    #[cfg(windows)]
    let pid_lock = None;
    // The setup that follows (`Store::open`, `restore_layout`, the accept loop)
    // can early-return via `?`. Own Unix pidfile + socket cleanup with a guard so
    // every exit path clears filesystem rendezvous state. Windows local sockets
    // are named-pipe endpoints owned by the listener; there is no socket path or
    // pidfile to unlink.
    let _daemon_files = DaemonFileGuard {
        #[cfg(unix)]
        socket_path: config.socket_path.clone(),
        _pid_lock: pid_lock,
    };

    let shutdown = Arc::new(AtomicBool::new(false));
    // The dispatcher drains a single ordered channel of `DispatchMsg`. PTY reader
    // threads emit `PtyEvent` (the `ManagedPty` contract), so a thin bridge
    // forwards each `PtyEvent` into the dispatcher channel as `DispatchMsg::Pty`.
    // A single FIFO bridge preserves output order; routing replay requests into
    // the SAME channel is what makes the dispatcher the single serialization
    // point for the output-vs-replay race (see `OutputBroadcaster`).
    let (dispatch_tx, dispatch_rx) = mpsc::channel::<DispatchMsg>();
    let (pty_tx, pty_rx) = mpsc::channel::<PtyEvent>();
    spawn_pty_bridge(pty_rx, dispatch_tx.clone());
    let store = Store::open(&config.store_path).map_err(io::Error::other)?;
    let state = Arc::new(Mutex::new(DaemonState::new(store, config.clone())));

    restore_layout(&state, &pty_tx).map_err(|err| io::Error::other(err.message))?;
    spawn_pty_dispatcher(Arc::clone(&state), dispatch_rx, Arc::clone(&shutdown));
    // Unix-only: the command poller drives the ADR 0011 dirty-exit backstop via
    // ManagedPty::foreground_command(), which is hard-coded to `None` on Windows
    // (ConPTY exposes no foreground process group — see hitch-pty and ADR 0011's
    // "Windows note"). With no resolvable command the poller can only re-broadcast
    // the same `None` already delivered on attach, while clear_stale_agent_state
    // returns early. Don't spawn the per-second no-op there.
    #[cfg(unix)]
    spawn_command_poller(Arc::clone(&state), Arc::clone(&shutdown));
    spawn_dirty_poller(Arc::clone(&state), Arc::clone(&shutdown));
    spawn_output_activity_poller(Arc::clone(&state), Arc::clone(&shutdown));

    // A dedicated thread parks in the blocking `accept()` and forwards each
    // accepted stream over this channel. Parking (rather than polling a
    // nonblocking listener) removes the poll-gap window in which a Windows
    // named-pipe client that connects, writes, and closes between polls was
    // silently dropped — there is always an armed accept waiting. The accept
    // thread re-checks `shutdown` after every `accept()` return and exits when it
    // is set; `ShutdownDaemon` wakes the parked accept with a best-effort
    // self-connect to its own endpoint (see the handler). A residual re-arm gap
    // remains between an `accept()` returning and the next one being issued, so a
    // concurrent connect can still see `ERROR_PIPE_BUSY` — clients keep their
    // busy-retry for that, and the hook keeps its ack-wait for stale daemons that
    // still poll (ADR 0012).
    let (accept_tx, accept_rx) = mpsc::channel::<DaemonStream>();
    let accept_shutdown = Arc::clone(&shutdown);
    let accept_thread = thread::Builder::new()
        .name("hitch-accept".to_string())
        .spawn(move || {
            // Exponential backoff for the error path: a hard, persistent failure
            // (e.g. fd/handle exhaustion — EMFILE/ENFILE/ERROR_NO_SYSTEM_RESOURCES)
            // leaves the listener handle valid and the shutdown flag unset, so
            // re-arming immediately would peg a CPU core and flood the log. Start
            // small, grow to a 1s ceiling, and reset on the next successful accept.
            const ACCEPT_BACKOFF_MIN: Duration = Duration::from_millis(25);
            const ACCEPT_BACKOFF_MAX: Duration = Duration::from_secs(1);
            let mut backoff = ACCEPT_BACKOFF_MIN;
            loop {
                match listener.accept() {
                    Ok(stream) => {
                        backoff = ACCEPT_BACKOFF_MIN;
                        if accept_shutdown.load(Ordering::SeqCst) {
                            // A shutdown self-connect (or a real client racing the
                            // flag) unparked us; stop arming new accepts.
                            break;
                        }
                        // The sole receiver is the main loop below; if it has gone
                        // (daemon already tearing down) the send fails and we exit.
                        if accept_tx.send(stream).is_err() {
                            break;
                        }
                    }
                    Err(err) => {
                        if accept_shutdown.load(Ordering::SeqCst) {
                            break;
                        }
                        // A per-accept failure (e.g. a transient named-pipe error)
                        // must not kill the daemon. Log and re-arm after a backoff
                        // sleep so a persistent failure (handle exhaustion) cannot
                        // spin a busy loop. Re-check shutdown after the sleep so a
                        // concurrent `ShutdownDaemon` is honored within one backoff
                        // interval rather than blocked behind it.
                        eprintln!("hitch-daemon: accept failed: {err}");
                        thread::sleep(backoff);
                        if accept_shutdown.load(Ordering::SeqCst) {
                            break;
                        }
                        backoff = (backoff * 2).min(ACCEPT_BACKOFF_MAX);
                    }
                }
            }
        })
        .map_err(io::Error::other)?;

    for stream in &accept_rx {
        if shutdown.load(Ordering::SeqCst) {
            break;
        }
        // Each accepted stream is switched to blocking for its per-client thread.
        // A failure here is per-connection (e.g. a Windows named-pipe handle
        // transiently rejecting the mode switch) and MUST NOT propagate out of
        // the accept loop — doing so would tear down the entire daemon (every PTY
        // session and all agent-state tracking) over one bad client. Log and drop
        // just this connection instead.
        if let Err(err) = stream.set_nonblocking(false) {
            eprintln!("hitch-daemon: dropping client, set_nonblocking failed: {err}");
            continue;
        }
        let client_id = register_client(&state, &stream)?;
        let state = Arc::clone(&state);
        let shutdown = Arc::clone(&shutdown);
        let channels = DispatchChannels {
            pty_tx: pty_tx.clone(),
            dispatch_tx: dispatch_tx.clone(),
        };
        thread::Builder::new()
            .name(format!("hitch-client-{client_id}"))
            .spawn(move || handle_client(client_id, stream, state, shutdown, channels))
            .map_err(io::Error::other)?;
    }

    // The accept thread observes the same `shutdown` flag and exits after its
    // current parked `accept()` is woken by the self-connect. Join it so a clean
    // shutdown does not race the listener's drop; detaching would also be safe
    // since the process exits, but joining keeps the lifecycle explicit.
    let _ = accept_thread.join();

    cancel_active_jobs(&state);
    wait_for_jobs_to_finish(&state);
    kill_all_sessions(&state);
    // Unix pidfile + socket are removed by `DaemonFileGuard` as it drops on return.
    Ok(())
}

/// Unblock the parked accept thread after the shutdown flag is set.
///
/// The accept thread sits in a blocking `accept()`; flipping `shutdown` is
/// invisible to it until a connection completes the pending accept. We complete
/// it ourselves with a best-effort connect to our own endpoint, then drop the
/// stream immediately. The accept thread wakes, re-reads `shutdown`, sees it set,
/// and exits without arming another accept. Failures are ignored: if the connect
/// loses a race with the listener already tearing down, the thread is exiting
/// anyway, and the daemon process exit reclaims the pipe regardless.
fn wake_accept_thread(socket_path: &Path) {
    // A few short attempts: the accept thread re-arms a fresh pipe instance after
    // each accepted connection, so a momentary `ERROR_PIPE_BUSY` (no armed
    // instance at this instant) clears as soon as it loops back into `accept()`.
    // Bounded so a daemon that already lost its endpoint never spins here.
    for _ in 0..10 {
        match connect_daemon(socket_path) {
            Ok(stream) => {
                drop(stream);
                return;
            }
            Err(err) if hitch_proto::transport::is_endpoint_busy(&err) => {
                thread::sleep(Duration::from_millis(10));
            }
            // NotFound / refused: the listener is already gone, so the accept
            // thread has already unblocked and exited. Nothing to wake.
            Err(_) => return,
        }
    }
}

/// Removes Unix daemon files whenever `run_daemon` returns — by normal shutdown
/// or by an early `?` during startup. Created right after the pidfile is written
/// so no exit path can leak a stale pid (see `write_pidfile`).
struct DaemonFileGuard {
    #[cfg(unix)]
    socket_path: PathBuf,
    // Held open for the daemon's whole lifetime so the advisory pidfile lock stays
    // taken; dropping it (clean exit or unwind) releases the lock, and the OS
    // releases it on an unclean kill too. Never read — only its lifetime matters.
    _pid_lock: Option<File>,
}

impl Drop for DaemonFileGuard {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            let _ = fs::remove_file(hitch_proto::transport::pidfile_path(&self.socket_path));
            let _ = fs::remove_file(&self.socket_path);
        }
    }
}

/// Write our pid to the daemon's pidfile (see `transport::pidfile_path`) and take
/// an exclusive advisory lock on it, held via the returned handle for our whole
/// lifetime. The lock is what lets a client tell a *live* daemon (lock held) from
/// a *stale* pidfile left by an unclean exit (lock free) before it force-kills the
/// pid named there — without it, PID reuse could send the kill to an unrelated
/// process. The OS drops the lock when this process exits by any means, so an
/// abrupt kill can't strand it.
///
/// The pidfile is the GUI's only handle on a daemon it could not handshake with,
/// so a stale entry must never linger: `truncate` overwrites any prior pid, and
/// the bind that precedes this call guarantees we are the sole owner of this
/// socket path. Any failure here only disables forced recovery; it never blocks
/// startup, so we return `None` and carry on.
#[cfg(unix)]
fn write_pidfile(socket_path: &Path) -> io::Result<Option<File>> {
    let path = hitch_proto::transport::pidfile_path(socket_path);
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&path)?;
    // SAFETY: `flock` on a freshly opened, valid fd. `LOCK_NB` so a contended lock
    // fails fast instead of blocking startup. We already own the socket bind, so
    // contention is not expected; if it happens, skip writing a pid we can't defend
    // with the lock rather than leave a kill-target we don't own.
    let locked = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0;
    if !locked {
        return Ok(None);
    }
    write!(file, "{}", std::process::id())?;
    file.flush()?;
    Ok(Some(file))
}

struct DaemonState {
    next_client_id: u64,
    clients: HashMap<u64, Arc<ClientSink>>,
    sessions: HashMap<SessionId, DaemonSession>,
    projects: HashMap<ProjectId, Project>,
    worktrees: HashMap<WorktreeId, Worktree>,
    store: Store,
    config: DaemonConfig,
    git: GitClient,
    /// The dispatcher thread's authoritative output log + readiness gates. Only
    /// the dispatcher mutates this, so its borrows of `DaemonState` are brief and
    /// never overlap a client thread's mutation of the same field.
    broadcaster: OutputBroadcaster,
    /// Live async **Jobs** keyed by id (ADR 0008). Each entry keeps the shared
    /// [`JobControl`] plus the UI metadata a late-attaching client needs to
    /// rebuild its live Job store before later `JobCompleted` arrives.
    jobs: HashMap<JobId, ActiveJob>,
}

impl DaemonState {
    fn new(store: Store, config: DaemonConfig) -> Self {
        let git = GitClient::with_programs(config.git.clone(), config.gh.clone());
        Self {
            next_client_id: 0,
            clients: HashMap::new(),
            sessions: HashMap::new(),
            projects: HashMap::new(),
            worktrees: HashMap::new(),
            store,
            config,
            git,
            broadcaster: OutputBroadcaster::default(),
            jobs: HashMap::new(),
        }
    }
}

struct ActiveJob {
    control: Arc<JobControl>,
    kind: Option<&'static str>,
    message: Option<String>,
}

/// Shared control handle for one running **Job** (ADR 0008). The Job registry on
/// `DaemonState` and the Job worker thread both hold an `Arc<JobControl>`:
/// `CancelJob` flips `cancelled` and terminates any registered process tree;
/// the worker checks `is_cancelled()` and registers the cancellable child tree
/// (the Draft Generator's provider tree, or git/gh commands) so cancellation
/// reaches grandchildren. On Windows the tree is a Job Object; on Unix it is a
/// process group.
#[derive(Default)]
struct JobControl {
    cancelled: AtomicBool,
    process_tree: Mutex<Option<ProcessTree>>,
}

impl JobControl {
    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    /// Register (or clear) the process tree the Job is running, so a concurrent
    /// cancel can terminate it. The worker sets it just after spawn and clears it
    /// on exit.
    fn set_process_tree(&self, process_tree: Option<ProcessTree>) {
        if let Ok(mut guard) = self.process_tree.lock() {
            *guard = process_tree;
        }
    }

    /// Signal cancellation and, if a child process tree is registered, terminate
    /// it. Jobs that run a subprocess-backed `git`/`gh` command register that
    /// tree too, so daemon shutdown can cancel them before exit rather than
    /// orphaning background work.
    fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        let process_tree = self
            .process_tree
            .lock()
            .ok()
            .and_then(|guard| guard.clone());
        if let Some(process_tree) = process_tree {
            let _ = process_tree.terminate();
        }
    }
}
impl CommandControl for JobControl {
    fn is_cancelled(&self) -> bool {
        JobControl::is_cancelled(self)
    }

    fn set_process_tree(&self, process_tree: Option<ProcessTree>) {
        JobControl::set_process_tree(self, process_tree);
    }
}

struct DaemonSession {
    session: Session,
    pty: Arc<ManagedPty>,
    /// Bytes to prepend ahead of the live broadcast log when a client replays
    /// this session. Always empty since ADR 0003 reopens restored sessions as
    /// fresh terminals (no cross-restart scrollback); kept as the seam the
    /// broadcaster's `replay_snapshot` composes against.
    restored_scrollback: Vec<u8>,
    agent: Option<KnownAgent>,
    agent_state: Option<AgentState>,
    agent_detail: Option<String>,
    agent_report_requires_running: bool,
    /// Output-activity gate (ADR 0011 amendment 2026-06-05): whether this
    /// session's PTY produced output within the last [`OUTPUT_ACTIVE_QUIET`].
    /// Edge-triggered — the dispatcher flips it `true` and broadcasts on the
    /// first frame after a quiet period; the output-activity poller flips it
    /// `false` and broadcasts once the session goes quiet. It gates the `WORKING`
    /// display word (`running ∧ output_active`) downstream.
    output_active: bool,
    /// Instant of the most recent output frame, or `None` if no output has been
    /// seen yet. The poller compares `now - last_output_at` against
    /// [`OUTPUT_ACTIVE_QUIET`] to drive the falling edge.
    last_output_at: Option<Instant>,
}

/// The dispatcher thread's authoritative, per-session record of "what has been
/// broadcast so far", plus the set of clients whose output gate is open.
///
/// This is the fix for the reconnect data-loss race (ADR 0007). The old replay
/// snapshotted the PTY reader ring (`pty.scrollback()`), which sits at a
/// DIFFERENT pipeline stage than the broadcast point: the reader appends to the
/// ring, *then* queues a `PtyEvent::Output` on the mpsc channel, and the
/// dispatcher broadcasts that event later. On reconnect a live `SessionOutput`
/// could reach a client before its replayed `SessionOpened` (which the desktop
/// uses to reset its byte ring), wiping those bytes; and a naive ring snapshot
/// would *also* re-broadcast bytes still queued in the channel, duplicating them.
///
/// By appending to this log on the SAME thread that broadcasts — the single
/// dispatcher thread — the log equals exactly the bytes already sent live. A
/// replay runs on that thread too, so the snapshot it sends and the live stream
/// that follows it cannot interleave: every byte lands in a client's snapshot OR
/// in a post-replay broadcast, never both, never neither.
///
/// The per-session live log mirrors the reader ring: it holds only bytes the
/// dispatcher has seen, bounded to the same capacity (`DEFAULT_SCROLLBACK_CAPACITY`,
/// trimmed at the head on overflow). Restored scrollback stays a separate buffer
/// prepended at replay time, exactly as the old `restored_scrollback +
/// pty.scrollback()` composition did.
#[derive(Default)]
struct OutputBroadcaster {
    /// Per-session live broadcast log: the bytes the dispatcher has broadcast,
    /// bounded to `DEFAULT_SCROLLBACK_CAPACITY` like the reader ring it mirrors.
    logs: HashMap<SessionId, VecDeque<u8>>,
    /// Clients whose output gate is open (replay has completed for them).
    live_clients: HashSet<u64>,
}

impl OutputBroadcaster {
    /// Append freshly broadcast `bytes` to a session's live log, trimming the
    /// head so the log never exceeds `DEFAULT_SCROLLBACK_CAPACITY`. The log
    /// holds live bytes only; restored scrollback is prepended in `replay_snapshot`.
    fn record_output(&mut self, session_id: SessionId, bytes: &[u8]) {
        let log = self.logs.entry(session_id).or_default();
        log.extend(bytes.iter().copied());
        if log.len() > DEFAULT_SCROLLBACK_CAPACITY {
            let overflow = log.len() - DEFAULT_SCROLLBACK_CAPACITY;
            log.drain(..overflow);
        }
    }

    /// The bytes a replaying client must receive for one session: the restored
    /// scrollback first, then the live log. This reproduces the old
    /// `restored_scrollback + pty.scrollback()` bytes, minus the in-flight gap
    /// the race exposed — because the log is appended on the dispatcher thread,
    /// it never contains bytes still queued in the mpsc channel.
    fn replay_snapshot(&self, session_id: SessionId, restored: &[u8]) -> Vec<u8> {
        let mut snapshot = restored.to_vec();
        if let Some(log) = self.logs.get(&session_id) {
            snapshot.extend(log.iter().copied());
        }
        snapshot
    }

    /// Open a client's output gate. Called by the dispatcher immediately after it
    /// has replayed every session's snapshot to the client, so the next `Output`
    /// it processes is the first one broadcast live to this client.
    fn mark_live(&mut self, client_id: u64) {
        self.live_clients.insert(client_id);
    }

    /// Whether a client's output gate is open. The live daemon reads the
    /// authoritative `ClientSink.live` atomic on the broadcast path (so it never
    /// re-locks state per sink); this mirror of the gate exists only so the
    /// `OutputBroadcaster` is a self-contained, single-threaded model the unit
    /// tests can drive — hence it is consulted only under `cfg(test)`.
    #[cfg(test)]
    fn is_live(&self, client_id: u64) -> bool {
        self.live_clients.contains(&client_id)
    }

    /// Forget a disconnected client's gate so the set doesn't grow unbounded.
    fn forget_client(&mut self, client_id: u64) {
        self.live_clients.remove(&client_id);
    }

    /// Drop a closed session's live log. The session id is never reused, so its
    /// log is dead weight once the session exits.
    fn forget_session(&mut self, session_id: SessionId) {
        self.logs.remove(&session_id);
    }
}

/// A message on the dispatcher's mpsc channel. `Pty` wraps the PTY reader's
/// events; `ReplayToClient` is enqueued by `handle_client` after it answers a
/// client's `Hello`. Routing the replay through the same queue makes the
/// dispatcher the single serialization point: a replay is processed in mpsc
/// order relative to `PtyEvent::Output`, so output appended before the replay
/// is in the snapshot and output appended after it is broadcast live — with no
/// gap and no duplication.
enum DispatchMsg {
    Pty(PtyEvent),
    ReplayToClient { client_id: u64 },
}

impl From<PtyEvent> for DispatchMsg {
    fn from(event: PtyEvent) -> Self {
        DispatchMsg::Pty(event)
    }
}

/// The two senders a client thread needs to drive the dispatcher pipeline.
/// `pty_tx` spawns new sessions' reader threads, whose `PtyEvent`s reach the
/// dispatcher via the bridge; `dispatch_tx` enqueues this client's replay into
/// the SAME ordered queue as output, which is what serializes replay against
/// live broadcasts (see `OutputBroadcaster`). They always travel together, so
/// they ride as one handle rather than as two parallel parameters.
#[derive(Clone)]
struct DispatchChannels {
    pty_tx: mpsc::Sender<PtyEvent>,
    dispatch_tx: mpsc::Sender<DispatchMsg>,
}

struct ClientSink {
    writer: Mutex<DaemonStream>,
    /// Output readiness gate. `false` until the replay thread has delivered the
    /// full scrollback snapshot and drained any output buffered in `pending`.
    /// and writes directly once it is open. Job events use their own gate below;
    /// all other control-plane broadcasts ignore this one (ADR 0007).
    live: AtomicBool,
    /// Job-event readiness gate. `false` until the reconnect replay has sent the
    /// current running-job snapshot and drained any `JobProgress`/`JobCompleted`
    /// events that raced with it.
    jobs_live: AtomicBool,
    /// Output buffered while the gate is closed. The replay thread holds this
    /// lock while draining and writing the buffered bytes, then sets `live=true`
    /// before releasing — guaranteeing snapshot → pending → live order with no
    /// gap or duplication.
    pending: Mutex<Vec<(SessionId, Vec<u8>)>>,
    /// Job events buffered while `jobs_live` is closed. The replay path drains
    /// this under the lock before opening the gate, so late-attaching clients see
    /// running-job snapshots before any raced completion/cancellation.
    pending_job_events: Mutex<Vec<Event>>,
    /// Agent-state readiness gate. `false` until reconnect replay has sent the
    /// per-session agent snapshots embedded in `SessionOpened`; raced live
    /// `AgentState` events wait here so a stale replay snapshot cannot regress
    /// the attaching client's newer view.
    agent_state_live: AtomicBool,
    /// Agent-state events buffered while `agent_state_live` is closed.
    pending_agent_state_events: Mutex<Vec<Event>>,
}

fn restore_layout(
    state: &Arc<Mutex<DaemonState>>,
    pty_tx: &mpsc::Sender<PtyEvent>,
) -> Result<(), ProtocolError> {
    let (projects, worktrees, sessions) = {
        let mut state = state.lock().map_err(|_| internal("state lock poisoned"))?;
        let layout = state.store.load_layout().map_err(store_error)?;
        state.projects = layout
            .projects
            .iter()
            .map(|project| (project.id, project.clone()))
            .collect();
        state.worktrees = layout
            .worktrees
            .iter()
            .map(|worktree| (worktree.id, worktree.clone()))
            .collect();
        (layout.projects, layout.worktrees, layout.sessions)
    };

    let _ = (projects, worktrees);
    for session in sessions {
        if !session.cwd.is_dir() {
            continue;
        }
        // Restored sessions must reinstall agent hooks just like freshly opened
        // ones (`open_session`): the hook configs live on disk in the worktree
        // and can be deleted between runs (the agent rewriting its config dir, a
        // clean checkout, manual cleanup). Without this, a restored session runs
        // agents that never report state until the user happens to open a new
        // session in the same worktree. Best-effort for the same reason as every
        // other install site: a broken config must not block restoring terminals.
        if let SessionParent::Worktree(worktree_id) = session.parent {
            if let Err(err) = install_agent_hooks_for_worktree_id(state, worktree_id) {
                eprintln!("hitch-daemon: {}", err.message);
            }
        }
        // ADR 0003: across a daemon restart the live PTY processes are gone, so
        // each saved session reopens as a FRESH terminal. We deliberately do NOT
        // replay the previous run's persisted scrollback. The respawned shell
        // prints its own banner/prompt; prepending the old run's transcript on
        // top stacked two banners (old + new), which read as duplicated output.
        // The in-memory broadcast log still drives same-daemon reattach; only
        // cross-restart history is dropped.
        let pty = ManagedPty::spawn(
            PtySpawnConfig::new(session.id, session.cwd.clone()).command(None),
            pty_tx.clone(),
        )
        .map_err(|err| ProtocolError::new(ErrorCode::PtyFailed, err.to_string()))?;
        let mut state = state.lock().map_err(|_| internal("state lock poisoned"))?;
        state.sessions.insert(
            session.id,
            DaemonSession {
                session,
                pty,
                restored_scrollback: Vec::new(),
                agent: None,
                agent_state: None,
                agent_detail: None,
                agent_report_requires_running: false,
                output_active: false,
                last_output_at: None,
            },
        );
    }
    Ok(())
}

fn register_client(state: &Arc<Mutex<DaemonState>>, stream: &DaemonStream) -> io::Result<u64> {
    let writer = stream.try_clone()?;
    let mut state = state.lock().map_err(|_| poisoned("state"))?;
    let client_id = state.next_client_id;
    state.next_client_id += 1;
    state.clients.insert(
        client_id,
        Arc::new(ClientSink {
            writer: Mutex::new(writer),
            live: AtomicBool::new(false),
            jobs_live: AtomicBool::new(false),
            pending: Mutex::new(Vec::new()),
            pending_job_events: Mutex::new(Vec::new()),
            agent_state_live: AtomicBool::new(false),
            pending_agent_state_events: Mutex::new(Vec::new()),
        }),
    );
    Ok(client_id)
}

fn unregister_client(state: &Arc<Mutex<DaemonState>>, client_id: u64) {
    if let Ok(mut state) = state.lock() {
        state.clients.remove(&client_id);
        state.broadcaster.forget_client(client_id);
    }
}

fn handle_client(
    client_id: u64,
    stream: DaemonStream,
    state: Arc<Mutex<DaemonState>>,
    shutdown: Arc<AtomicBool>,
    channels: DispatchChannels,
) {
    let mut reader = BufReader::new(stream);

    loop {
        let line = match read_control_line(&mut reader) {
            Ok(Some(line)) => line,
            Ok(None) => break,
            Err(err) => {
                let _ = send_response(
                    &state,
                    client_id,
                    0,
                    Response::Error {
                        error: ProtocolError::new(ErrorCode::InvalidRequest, err.to_string()),
                    },
                );
                break;
            }
        };

        let message: ControlMessage = match serde_json::from_slice(&line) {
            Ok(message) => message,
            Err(err) => {
                let _ = send_response(
                    &state,
                    client_id,
                    request_id_from_control_line(&line).unwrap_or(0),
                    Response::Error {
                        error: ProtocolError::new(ErrorCode::InvalidRequest, err.to_string()),
                    },
                );
                continue;
            }
        };

        let ControlMessage::Request { id, request } = message else {
            continue;
        };

        if let Err(error) = handle_request(
            &mut reader,
            &state,
            client_id,
            id,
            request,
            &shutdown,
            &channels,
        ) {
            let _ = send_response(&state, client_id, id, Response::Error { error });
        }

        if shutdown.load(Ordering::SeqCst) {
            break;
        }
    }

    unregister_client(&state, client_id);
}

fn handle_request<R: Read>(
    reader: &mut R,
    state: &Arc<Mutex<DaemonState>>,
    client_id: u64,
    request_id: u64,
    request: Request,
    shutdown: &Arc<AtomicBool>,
    channels: &DispatchChannels,
) -> Result<(), ProtocolError> {
    match request {
        Request::Hello {
            protocol_version, ..
        } => {
            if protocol_version != PROTOCOL_VERSION {
                send_response(
                    state,
                    client_id,
                    request_id,
                    Response::Error {
                        error: ProtocolError::new(
                            ErrorCode::UnsupportedProtocol,
                            format!(
                                "client protocol {protocol_version} != daemon protocol {PROTOCOL_VERSION}"
                            ),
                        ),
                    },
                )?;
                return Ok(());
            }
            send_response(
                state,
                client_id,
                request_id,
                Response::Hello {
                    protocol_version: PROTOCOL_VERSION,
                    daemon_pid: std::process::id(),
                },
            )?;
            // Replay must run ON the dispatcher thread so the snapshot it sends
            // and the live output that follows it are serialized through the same
            // queue (see `OutputBroadcaster`). Enqueue the request rather than
            // replaying inline; if the dispatcher has gone, the client simply
            // gets no scrollback, which is no worse than a disconnect.
            let _ = channels
                .dispatch_tx
                .send(DispatchMsg::ReplayToClient { client_id });
        }
        Request::ListProjects => {
            let projects = {
                let state = state.lock().map_err(|_| internal("state lock poisoned"))?;
                state.store.list_projects().map_err(store_error)?
            };
            send_response(
                state,
                client_id,
                request_id,
                Response::Projects { projects },
            )?;
        }
        Request::AddProject { root } => {
            let project = add_project_from_root(state, &root, None)?;
            send_response(
                state,
                client_id,
                request_id,
                Response::Projects {
                    projects: vec![project.clone()],
                },
            )?;
            broadcast_event(state, Event::ProjectUpdated { project })?;
        }
        Request::CloneProject { .. } => {
            let request = JobRequest::try_from(request)
                .map_err(|_| internal("job-capable request rejected during dispatch"))?;
            dispatch_job(state, client_id, request_id, request)?;
        }
        Request::RemoveProject { project_id, force } => {
            let closed_session_ids = remove_project(state, project_id, force)?;
            send_response(state, client_id, request_id, Response::Ack)?;
            // close_session does not itself broadcast; mirror the CloseSession
            // handler so peer clients drop each killed session too.
            for session_id in closed_session_ids {
                broadcast_event(
                    state,
                    Event::SessionClosed {
                        session_id,
                        exit_code: None,
                    },
                )?;
            }
            broadcast_event(state, Event::ProjectRemoved { project_id })?;
        }
        Request::ListBranches { project_id } => {
            let branches = list_branches(state, project_id)?;
            send_response(
                state,
                client_id,
                request_id,
                Response::Branches { branches },
            )?;
        }
        Request::ListWorktrees { project_id } => {
            let worktrees = list_worktrees(state, project_id)?;
            send_response(
                state,
                client_id,
                request_id,
                Response::Worktrees { worktrees },
            )?;
        }
        Request::CreateWorktree { .. } => {
            let request = JobRequest::try_from(request)
                .map_err(|_| internal("job-capable request rejected during dispatch"))?;
            dispatch_job(state, client_id, request_id, request)?;
        }
        Request::RemoveWorktree {
            worktree_id,
            delete_branch,
            force,
        } => {
            let closed_session_ids = remove_worktree(state, worktree_id, delete_branch, force)?;
            send_response(state, client_id, request_id, Response::Ack)?;
            for session_id in closed_session_ids {
                broadcast_event(
                    state,
                    Event::SessionClosed {
                        session_id,
                        exit_code: None,
                    },
                )?;
            }
            broadcast_event(state, Event::WorktreeRemoved { worktree_id })?;
        }
        Request::ListSessions { parent } => {
            let sessions = {
                let state = state.lock().map_err(|_| internal("state lock poisoned"))?;
                state
                    .sessions
                    .values()
                    .filter(|daemon_session| {
                        parent.is_none_or(|p| daemon_session.session.parent == p)
                    })
                    .map(|daemon_session| daemon_session.session.clone())
                    .collect()
            };
            send_response(
                state,
                client_id,
                request_id,
                Response::Sessions { sessions },
            )?;
        }
        Request::OpenSession {
            parent,
            name,
            command,
            cols,
            rows,
        } => {
            let session = open_session(state, parent, name, command, cols, rows, &channels.pty_tx)?;
            let replay = session_opened_replay(state, session.id)?;
            send_response(
                state,
                client_id,
                request_id,
                Response::SessionOpened {
                    session: session.clone(),
                    agent: replay.agent,
                    agent_state: replay.agent_state,
                    agent_detail: replay.agent_detail.clone(),
                    output_active: replay.output_active,
                },
            )?;
            broadcast_event(
                state,
                Event::SessionOpened {
                    session,
                    agent: replay.agent,
                    agent_state: replay.agent_state,
                    agent_detail: replay.agent_detail,
                    output_active: replay.output_active,
                },
            )?;
        }
        Request::SendSessionInput {
            session_id,
            byte_count,
        } => {
            let payload = read_pty_payload(reader)?;
            if payload.len() != byte_count as usize {
                return Err(ProtocolError::new(
                    ErrorCode::InvalidRequest,
                    format!(
                        "input frame length {} did not match announced byte_count {byte_count}",
                        payload.len()
                    ),
                ));
            }
            let pty = find_pty(state, session_id)?;
            pty.write_input(&payload)
                .map_err(|err| ProtocolError::new(ErrorCode::PtyFailed, err.to_string()))?;
            send_response(state, client_id, request_id, Response::Ack)?;
        }
        Request::ResizeSession {
            session_id,
            cols,
            rows,
        } => {
            let pty = find_pty(state, session_id)?;
            pty.resize(TerminalSize::new(cols, rows))
                .map_err(|err| ProtocolError::new(ErrorCode::PtyFailed, err.to_string()))?;
            send_response(state, client_id, request_id, Response::Ack)?;
        }
        Request::RepaintSession { session_id } => {
            let pty = find_pty(state, session_id)?;
            pty.repaint()
                .map_err(|err| ProtocolError::new(ErrorCode::PtyFailed, err.to_string()))?;
            send_response(state, client_id, request_id, Response::Ack)?;
        }
        Request::CloseSession {
            session_id,
            kill_process,
        } => {
            close_session(state, session_id, kill_process)?;
            send_response(state, client_id, request_id, Response::Ack)?;
            broadcast_event(
                state,
                Event::SessionClosed {
                    session_id,
                    exit_code: None,
                },
            )?;
        }
        Request::RenameSession { session_id, name } => {
            rename_session(state, session_id, name)?;
            send_response(state, client_id, request_id, Response::Ack)?;
        }
        Request::GitStatus { worktree_id } => {
            let status = git_status(state, worktree_id)?;
            send_response(state, client_id, request_id, Response::GitStatus { status })?;
        }
        Request::GitDiff { worktree_id, path } => {
            let diff = git_diff(state, worktree_id, path)?;
            send_response(state, client_id, request_id, Response::FileDiff { diff })?;
        }
        Request::StageFiles { worktree_id, paths } => {
            let (git, worktree_path) = git_context(state, worktree_id)?;
            git.stage_files(&worktree_path, &paths).map_err(git_error)?;
            send_response(state, client_id, request_id, Response::Ack)?;
            broadcast_dirty(state, worktree_id)?;
        }
        Request::UnstageFiles { worktree_id, paths } => {
            let (git, worktree_path) = git_context(state, worktree_id)?;
            git.unstage_files(&worktree_path, &paths)
                .map_err(git_error)?;
            send_response(state, client_id, request_id, Response::Ack)?;
            broadcast_dirty(state, worktree_id)?;
        }
        Request::DiscardFiles { worktree_id, paths } => {
            let (git, worktree_path) = git_context(state, worktree_id)?;
            git.discard_files(&worktree_path, &paths)
                .map_err(git_error)?;
            send_response(state, client_id, request_id, Response::Ack)?;
            broadcast_dirty(state, worktree_id)?;
        }
        Request::Commit {
            worktree_id,
            subject,
            body,
        } => {
            let (git, worktree_path) = git_context(state, worktree_id)?;
            git.commit(&worktree_path, &subject, body.as_deref())
                .map_err(git_error)?;
            send_response(state, client_id, request_id, Response::Ack)?;
            broadcast_dirty(state, worktree_id)?;
        }
        // Long-running operations run off the request loop as Jobs (ADR 0008).
        // The bare requests and the explicit `StartJob` wrapper share one
        // dispatch path; both reply `JobStarted` and broadcast lifecycle events.
        Request::ListDraftModels { .. }
        | Request::GenerateCommitDraft { .. }
        | Request::GeneratePullRequestDraft { .. }
        | Request::Push { .. }
        | Request::Fetch { .. }
        | Request::Pull { .. }
        | Request::PrStatus { .. }
        | Request::ProjectPrStatuses { .. }
        | Request::CreatePullRequest { .. } => {
            let request = JobRequest::try_from(request)
                .map_err(|_| internal("job-capable request rejected during dispatch"))?;
            dispatch_job(state, client_id, request_id, request)?;
        }
        Request::StartJob { request } => {
            dispatch_job(state, client_id, request_id, request)?;
        }
        Request::CancelJob { job_id } => {
            let control = {
                let guard = state.lock().map_err(|_| internal("state lock poisoned"))?;
                guard.jobs.get(&job_id).map(|job| Arc::clone(&job.control))
            };
            if let Some(control) = control {
                control.cancel();
            }
            // Cancelling an unknown/finished Job is a no-op success: the worker
            // may have already completed and removed itself from the registry.
            send_response(state, client_id, request_id, Response::Ack)?;
        }
        Request::Ping => {
            send_response(state, client_id, request_id, Response::Pong)?;
        }
        Request::InstallAgentHooks { worktree_id } => {
            let (path, helper, git) = {
                let state = state.lock().map_err(|_| internal("state lock poisoned"))?;
                let worktree = state
                    .worktrees
                    .get(&worktree_id)
                    .ok_or_else(|| ProtocolError::new(ErrorCode::NotFound, "worktree not found"))?;
                (
                    worktree.path.clone(),
                    state.config.hook_helper.clone(),
                    state.config.git.clone(),
                )
            };
            hitch_agent::install_hooks(&path, &HookInstallOptions::new(helper).with_git(git))
                .map_err(|err| ProtocolError::new(ErrorCode::AgentHookFailed, err.to_string()))?;
            send_response(state, client_id, request_id, Response::Ack)?;
        }
        Request::ReportAgentState {
            agent,
            state: agent_state,
            session_id,
            cwd,
            detail,
        } => {
            if let Some(event) = store_agent_report(
                state,
                agent,
                agent_state,
                session_id,
                cwd.as_deref(),
                detail,
            )? {
                broadcast_agent_state_event(
                    state,
                    Event::AgentState {
                        session_id: Some(event.session_id),
                        worktree_id: event.worktree_id,
                        agent: event.agent,
                        state: event.state,
                        detail: event.detail,
                    },
                )?;
            }
            send_response(state, client_id, request_id, Response::Ack)?;
        }
        Request::AnnounceAgent {
            agent,
            session_id,
            cwd,
        } => {
            // Identity-only announce: store *which* agent runs in the session so
            // the Session mark can render before the first prompt. Bypasses the
            // late-arrival guard and never sets/changes Agent State (ADR 0011
            // amendment 2026-06-05). Propagate a changed identity to attached
            // clients via the same `AgentState` event path state reports use —
            // there is no separate identity event type.
            if let Some(event) = store_agent_announce(state, agent, session_id, cwd.as_deref())? {
                broadcast_agent_state_event(
                    state,
                    Event::AgentState {
                        session_id: Some(event.session_id),
                        worktree_id: event.worktree_id,
                        agent: event.agent,
                        state: event.state,
                        detail: event.detail,
                    },
                )?;
            }
            send_response(state, client_id, request_id, Response::Ack)?;
        }
        Request::ShutdownDaemon => {
            send_response(state, client_id, request_id, Response::Ack)?;
            // Resolve the endpoint before flipping the flag so the wake connect
            // below targets our own listener.
            let socket_path = {
                let guard = state.lock().map_err(|_| internal("state lock poisoned"))?;
                guard.config.socket_path.clone()
            };
            shutdown.store(true, Ordering::SeqCst);
            wake_accept_thread(&socket_path);
        }
    }

    Ok(())
}

fn add_project_from_root(
    state: &Arc<Mutex<DaemonState>>,
    root: &Path,
    explicit_name: Option<&str>,
) -> Result<Project, ProtocolError> {
    let canonical = root
        .canonicalize()
        .map_err(|err| ProtocolError::new(ErrorCode::InvalidRequest, err.to_string()))?;
    if !canonical.is_dir() {
        return Err(ProtocolError::new(
            ErrorCode::InvalidRequest,
            format!("project root is not a directory: {}", canonical.display()),
        ));
    }

    let (project_root, kind, branch) = match GitRepository::discover(&canonical) {
        Ok(repo) => {
            let branch = repo.current_branch().unwrap_or_else(|_| "HEAD".into());
            (
                repo.root().to_path_buf(),
                ProjectKind::GitBacked,
                Some(branch),
            )
        }
        Err(_) => (canonical, ProjectKind::Plain, None),
    };
    let name = explicit_name
        .map(str::to_owned)
        .or_else(|| {
            project_root
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "Project".into());
    let project = Project::new(name, project_root.clone(), kind);

    let main_worktree =
        branch.map(|branch| Worktree::new(project.id, project_root, branch, true, false));
    let (hook_helper, git) = {
        let state = state.lock().map_err(|_| internal("state lock poisoned"))?;
        (state.config.hook_helper.clone(), state.config.git.clone())
    };
    // Hook installation is best-effort: a malformed or unwritable agent config
    // must not block adding an otherwise usable repository. Agent-state tracking
    // degrades until the config is fixed; opening/reconciling reinstalls hooks.
    // This mirrors the create-worktree and session-open paths below.
    if let Some(worktree) = &main_worktree {
        if let Err(err) =
            install_agent_hooks_for_worktree_path(&project.root, &worktree.path, &hook_helper, &git)
        {
            eprintln!("hitch-daemon: {}", err.message);
        }
    }
    let mut state = state.lock().map_err(|_| internal("state lock poisoned"))?;
    state.store.insert_project(&project).map_err(store_error)?;
    state.projects.insert(project.id, project.clone());
    if let Some(worktree) = main_worktree {
        state
            .store
            .insert_worktree(&worktree)
            .map_err(store_error)?;
        state.worktrees.insert(worktree.id, worktree);
    }
    Ok(project)
}

fn list_worktrees(
    state: &Arc<Mutex<DaemonState>>,
    project_id: ProjectId,
) -> Result<Vec<Worktree>, ProtocolError> {
    // Pick up any worktrees git knows about that Hitch hasn't registered yet,
    // and prune tracked linked worktrees whose directory is now gone. Listing
    // runs on add-project, clone, create, and every GUI refresh/reconnect, so
    // this is the single place external worktrees get reconciled. Best-effort:
    // a discovery failure must not break listing.
    reconcile_discovered_worktrees(state, project_id);

    let worktrees = {
        let state = state.lock().map_err(|_| internal("state lock poisoned"))?;
        state
            .store
            .list_worktrees(project_id)
            .map_err(store_error)?
    };

    let mut refreshed = Vec::with_capacity(worktrees.len());
    for worktree in worktrees {
        // Reconciliation is the single place an external branch change is
        // picked up and persisted. Broadcast it here so every attached GUI sees
        // the new branch, not just the one that later selects this worktree:
        // otherwise a background project-pr-statuses refresh updates the PR chip
        // by worktree id while the stale branch name lingers beside it. Mirrors
        // the single-worktree `refreshed_worktree_context` path.
        let (worktree, branch_changed) = refresh_worktree_branch_from_disk(state, worktree)?;
        if branch_changed {
            broadcast_event(
                state,
                Event::WorktreeUpdated {
                    worktree: worktree.clone(),
                },
            )?;
        }
        refreshed.push(worktree);
    }
    Ok(refreshed)
}

/// Canonicalize a path for identity comparison, falling back to the path as
/// given when it can't be resolved (e.g. it no longer exists on disk).
fn canonical_or_self(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn install_agent_hooks_for_worktree_id(
    state: &Arc<Mutex<DaemonState>>,
    worktree_id: WorktreeId,
) -> Result<(), ProtocolError> {
    let (project_root, worktree_path, helper, git) = {
        let state = state.lock().map_err(|_| internal("state lock poisoned"))?;
        let worktree = state
            .worktrees
            .get(&worktree_id)
            .ok_or_else(|| ProtocolError::new(ErrorCode::NotFound, "worktree not found"))?;
        let project = state
            .projects
            .get(&worktree.project_id)
            .ok_or_else(|| ProtocolError::new(ErrorCode::NotFound, "project not found"))?;
        (
            project.root.clone(),
            worktree.path.clone(),
            state.config.hook_helper.clone(),
            state.config.git.clone(),
        )
    };
    install_agent_hooks_for_worktree_path(&project_root, &worktree_path, &helper, &git)
}

fn install_agent_hooks_for_worktree_path(
    project_root: &Path,
    worktree_path: &Path,
    helper: &Path,
    git: &Path,
) -> Result<(), ProtocolError> {
    hitch_agent::install_hooks(
        worktree_path,
        &HookInstallOptions::new(helper).with_git(git),
    )
    .map(|_| ())
    .map_err(|err| {
        ProtocolError::new(
            ErrorCode::AgentHookFailed,
            format!(
                "failed to install agent hooks for worktree {} in project {}: {err}",
                worktree_path.display(),
                project_root.display()
            ),
        )
    })
}

/// Reconcile a git-backed project's stored worktrees against what git currently
/// reports: register newly discovered worktrees and prune tracked linked
/// worktrees whose directory is now gone or no longer belongs to the repo.
/// Matches existing worktrees by canonical path so the main worktree (inserted
/// at add-project time) and already-tracked linked worktrees are never
/// duplicated. Best-effort and silent: a non-git project, a discovery error,
/// or a store error leaves the current set as intact as possible rather than
/// failing the surrounding `list-worktrees`.
fn reconcile_discovered_worktrees(state: &Arc<Mutex<DaemonState>>, project_id: ProjectId) {
    let (root, stored_worktrees) = {
        let Ok(state) = state.lock() else { return };
        let Some(project) = state.projects.get(&project_id) else {
            return;
        };
        if project.kind != ProjectKind::GitBacked {
            return;
        }
        let stored = state.store.list_worktrees(project_id).unwrap_or_default();
        (project.root.clone(), stored)
    };

    let Ok(discovered) = GitRepository::discover(&root).and_then(|repo| repo.worktrees()) else {
        return;
    };

    let mut discovered_by_path = HashMap::new();
    for found in discovered {
        discovered_by_path.insert(canonical_or_self(&found.path), found);
    }

    for worktree in &stored_worktrees {
        if discovered_by_path.contains_key(&canonical_or_self(&worktree.path)) {
            continue;
        }
        prune_missing_worktree(state, worktree.id);
    }

    let existing_paths: HashSet<PathBuf> = stored_worktrees
        .iter()
        .map(|worktree| canonical_or_self(&worktree.path))
        .collect();
    for (path, found) in discovered_by_path {
        if existing_paths.contains(&path) {
            continue;
        }
        let worktree = Worktree::new(project_id, path, found.branch, found.is_main, false);
        let Ok((project_root, helper, git)) = ({
            let state = state.lock();
            state.map(|state| {
                (
                    state
                        .projects
                        .get(&project_id)
                        .map(|project| project.root.clone())
                        .unwrap_or_default(),
                    state.config.hook_helper.clone(),
                    state.config.git.clone(),
                )
            })
        }) else {
            return;
        };
        // Hook installation is best-effort: a malformed or unwritable agent
        // config must not hide an otherwise valid externally-created worktree
        // from list-worktrees. Record it regardless; agent-state tracking just
        // degrades until the config is fixed (reopening reinstalls hooks).
        if let Err(err) =
            install_agent_hooks_for_worktree_path(&project_root, &worktree.path, &helper, &git)
        {
            eprintln!("hitch-daemon: {}", err.message);
        }
        let Ok(mut state) = state.lock() else { return };
        if state.store.insert_worktree(&worktree).is_ok() {
            state.worktrees.insert(worktree.id, worktree);
        }
    }
}

fn prune_missing_worktree(state: &Arc<Mutex<DaemonState>>, worktree_id: WorktreeId) {
    let live_session_ids = {
        let Ok(state) = state.lock() else { return };
        state
            .sessions
            .values()
            .filter(|session| session.session.parent == SessionParent::Worktree(worktree_id))
            .map(|session| session.session.id)
            .collect::<Vec<_>>()
    };

    for session_id in live_session_ids {
        match close_session(state, session_id, true) {
            Ok(()) => {
                let _ = broadcast_event(
                    state,
                    Event::SessionClosed {
                        session_id,
                        exit_code: None,
                    },
                );
            }
            Err(err) if err.code == ErrorCode::NotFound => {}
            Err(_) => return,
        }
    }

    let Ok(mut guard) = state.lock() else { return };
    if guard.store.delete_worktree(worktree_id).is_ok() {
        guard.worktrees.remove(&worktree_id);
        drop(guard);
        let _ = broadcast_event(state, Event::WorktreeRemoved { worktree_id });
    }
}

fn list_branches(
    state: &Arc<Mutex<DaemonState>>,
    project_id: ProjectId,
) -> Result<Vec<hitch_proto::BranchSummary>, ProtocolError> {
    let project_root = {
        let state = state.lock().map_err(|_| internal("state lock poisoned"))?;
        let project = state
            .projects
            .get(&project_id)
            .cloned()
            .ok_or_else(|| ProtocolError::new(ErrorCode::NotFound, "project not found"))?;
        if project.kind != ProjectKind::GitBacked {
            return Err(ProtocolError::new(
                ErrorCode::InvalidRequest,
                "plain projects do not have branches",
            ));
        }
        project.root.clone()
    };
    let branch_infos = GitRepository::discover(&project_root)
        .and_then(|repo| repo.branches())
        .map_err(git_error)?;
    Ok(branch_infos
        .into_iter()
        .map(|b| hitch_proto::BranchSummary {
            name: b.name,
            is_remote: b.is_remote,
        })
        .collect())
}

fn refresh_worktree_branch_from_disk(
    state: &Arc<Mutex<DaemonState>>,
    mut worktree: Worktree,
) -> Result<(Worktree, bool), ProtocolError> {
    let branch =
        match GitRepository::discover(&worktree.path).and_then(|repo| repo.current_branch()) {
            Ok(branch) => branch,
            Err(_) => return Ok((worktree, false)),
        };
    if branch == worktree.branch {
        return Ok((worktree, false));
    }

    worktree.branch = branch;
    let mut state = state.lock().map_err(|_| internal("state lock poisoned"))?;
    state
        .store
        .update_worktree(&worktree)
        .map_err(store_error)?;
    state.worktrees.insert(worktree.id, worktree.clone());
    Ok((worktree, true))
}

fn clone_project(
    git: &GitClient,
    remote_url: &str,
    destination: &Path,
    name: Option<&str>,
    control: Option<&JobControl>,
) -> Result<PathBuf, ProtocolError> {
    let target = match name {
        Some(name) => destination.join(name),
        None => destination.to_path_buf(),
    };
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| ProtocolError::new(ErrorCode::InvalidRequest, err.to_string()))?;
    }
    match control {
        Some(control) => git.clone_repo_with_control(remote_url, &target, control),
        None => git.clone_repo(remote_url, &target),
    }
    .map_err(git_error)?;
    Ok(target)
}

fn create_worktree(
    state: &Arc<Mutex<DaemonState>>,
    project_id: ProjectId,
    branch: String,
    base: Option<String>,
    mode: WorktreeCreateMode,
    control: Option<&JobControl>,
) -> Result<Worktree, ProtocolError> {
    let (project, managed_root, git, hook_helper, git_path) = {
        let state = state.lock().map_err(|_| internal("state lock poisoned"))?;
        let project = state
            .projects
            .get(&project_id)
            .cloned()
            .ok_or_else(|| ProtocolError::new(ErrorCode::NotFound, "project not found"))?;
        (
            project,
            state.config.managed_root.clone(),
            state.git.clone(),
            state.config.hook_helper.clone(),
            state.config.git.clone(),
        )
    };
    if project.kind != ProjectKind::GitBacked {
        return Err(ProtocolError::new(
            ErrorCode::InvalidRequest,
            "plain projects do not support worktrees",
        ));
    }

    let request = CreateWorktreeRequest {
        project_id,
        project_name: project.name,
        managed_root,
        branch,
        checkout: match mode {
            WorktreeCreateMode::NewBranch => WorktreeCheckout::NewBranch,
            WorktreeCreateMode::ExistingBranch => WorktreeCheckout::ExistingBranch,
        },
        base,
    };
    let worktree = match control {
        Some(control) => git.create_worktree_with_control(&project.root, &request, control),
        None => git.create_worktree(&project.root, &request),
    }
    .map_err(git_error)?;
    // Hook installation is best-effort: if it fails we still record the worktree
    // so the checkout git just created never becomes an orphan that Hitch can't
    // see (which would conflict with retries on the same branch/path). Agent-state
    // tracking degrades until the config is fixed; reopening reinstalls hooks.
    if let Err(err) = install_agent_hooks_for_worktree_path(
        &project.root,
        &worktree.path,
        &hook_helper,
        &git_path,
    ) {
        eprintln!("hitch-daemon: {}", err.message);
    }
    let mut state = state.lock().map_err(|_| internal("state lock poisoned"))?;
    state
        .store
        .insert_worktree(&worktree)
        .map_err(store_error)?;
    state.worktrees.insert(worktree.id, worktree.clone());
    Ok(worktree)
}

fn remove_project(
    state: &Arc<Mutex<DaemonState>>,
    project_id: ProjectId,
    force: bool,
) -> Result<Vec<SessionId>, ProtocolError> {
    let (_worktree_ids, live_session_ids) = {
        let state = state.lock().map_err(|_| internal("state lock poisoned"))?;
        if !state.projects.contains_key(&project_id) {
            return Err(ProtocolError::new(ErrorCode::NotFound, "project not found"));
        }
        let worktree_ids = state
            .worktrees
            .values()
            .filter(|worktree| worktree.project_id == project_id)
            .map(|worktree| worktree.id)
            .collect::<Vec<_>>();
        let live_session_ids = state
            .sessions
            .values()
            .filter(|session| match session.session.parent {
                SessionParent::Project(id) => id == project_id,
                SessionParent::Worktree(id) => worktree_ids.contains(&id),
            })
            .map(|session| session.session.id)
            .collect::<Vec<_>>();
        (worktree_ids, live_session_ids)
    };

    if !force && !live_session_ids.is_empty() {
        return Err(ProtocolError::new(
            ErrorCode::LiveSessions,
            "project has live sessions; retry with force to kill them",
        ));
    }

    let mut closed_session_ids = Vec::new();
    for session_id in live_session_ids {
        match close_session(state, session_id, true) {
            Ok(()) => closed_session_ids.push(session_id),
            // The session may have exited on its own (PTY-exit dispatcher) or
            // been closed by another client between our snapshot and this kill.
            // A force-removal must not be derailed by an already-gone session.
            Err(err) if err.code == ErrorCode::NotFound => {}
            Err(err) => return Err(err),
        }
    }

    let mut state = state.lock().map_err(|_| internal("state lock poisoned"))?;

    // Another client can OpenSession under this project (or one of its
    // worktrees) in the gap between the pre-snapshot above and this final lock.
    // Recompute the live set under the lock we now hold through the delete so
    // the force decision and the cleanup see the same world.
    let worktree_ids = state
        .worktrees
        .values()
        .filter(|wt| wt.project_id == project_id)
        .map(|wt| wt.id)
        .collect::<Vec<_>>();
    let racing_ids = state
        .sessions
        .values()
        .filter(|session| match session.session.parent {
            SessionParent::Project(id) => id == project_id,
            SessionParent::Worktree(id) => worktree_ids.contains(&id),
        })
        .map(|session| session.session.id)
        .collect::<Vec<_>>();

    // Honor the force contract: a non-force removal must refuse rather than
    // silently terminate a session that raced in after the snapshot. Reaching
    // this point with `!force` means the snapshot was empty (else the guard
    // above returned), so nothing has been closed and the project is still
    // intact — failing here is a clean no-op.
    if !force && !racing_ids.is_empty() {
        return Err(ProtocolError::new(
            ErrorCode::LiveSessions,
            "project has live sessions; retry with force to kill them",
        ));
    }

    state
        .store
        .delete_project(project_id)
        .map_err(store_error)?;
    state.projects.remove(&project_id);
    for worktree_id in &worktree_ids {
        state.worktrees.remove(worktree_id);
    }

    // For a force removal, evict any raced-in session from the live map and
    // kill it alongside the snapshotted sessions; `delete_project` already
    // removed its store row (it deletes sessions by parent), so it would
    // otherwise survive as an orphaned, unreachable PTY pointing at a now-
    // deleted project.
    let orphans = racing_ids
        .into_iter()
        .filter_map(|id| state.sessions.remove(&id).map(|session| (id, session)))
        .collect::<Vec<_>>();
    drop(state);

    for (session_id, session) in orphans {
        let _ = session.pty.kill();
        closed_session_ids.push(session_id);
    }
    Ok(closed_session_ids)
}

fn remove_worktree(
    state: &Arc<Mutex<DaemonState>>,
    worktree_id: WorktreeId,
    delete_branch: bool,
    force: bool,
) -> Result<Vec<SessionId>, ProtocolError> {
    let (project, worktree, git_path, live_session_ids) = {
        let state = state.lock().map_err(|_| internal("state lock poisoned"))?;
        let worktree = state
            .worktrees
            .get(&worktree_id)
            .cloned()
            .ok_or_else(|| ProtocolError::new(ErrorCode::NotFound, "worktree not found"))?;
        let project = state
            .projects
            .get(&worktree.project_id)
            .cloned()
            .ok_or_else(|| ProtocolError::new(ErrorCode::NotFound, "project not found"))?;
        let live_session_ids = state
            .sessions
            .values()
            .filter(|session| session.session.parent == SessionParent::Worktree(worktree_id))
            .map(|session| session.session.id)
            .collect::<Vec<_>>();
        (
            project,
            worktree,
            state.config.git.clone(),
            live_session_ids,
        )
    };

    if worktree.is_main {
        return Err(ProtocolError::new(
            ErrorCode::InvalidRequest,
            "main worktree cannot be removed by Hitch",
        ));
    }
    if !worktree.is_hitch_managed {
        return Err(ProtocolError::new(
            ErrorCode::InvalidRequest,
            "externally managed worktrees cannot be removed by Hitch",
        ));
    }
    if !force && !live_session_ids.is_empty() {
        return Err(ProtocolError::new(
            ErrorCode::LiveSessions,
            "worktree has live sessions; retry with force to kill them",
        ));
    }
    if !force
        && GitRepository::discover(&worktree.path)
            .map_err(git_error)?
            .is_dirty()
            .map_err(git_error)?
    {
        return Err(ProtocolError::new(
            ErrorCode::DirtyWorktree,
            "worktree has uncommitted changes; retry with force after confirming",
        ));
    }

    // Tear down every session that lives in this worktree, then run git. We kill
    // the PTYs BEFORE git removal because on Windows a live shell holds the
    // worktree directory open and `git worktree remove` would fail. The
    // destructive teardown must be SURVIVABLE: git removal can still fail (the
    // directory is busy, the branch has unmerged commits), and a failed removal
    // must leave the worktree usable rather than orphaning its sessions. We
    // therefore capture each closed session's layout row and, on git failure,
    // re-insert those rows so the next daemon launch's `restore_layout` revives
    // them as fresh terminals (ADR 0003). We rely on restore rather than keeping
    // the dead PTYs in memory because killing a PTY makes its in-memory session
    // unusable anyway, and the PTY-exit dispatcher deletes such rows on its own.
    let mut closed = Vec::new();
    {
        let mut state = state.lock().map_err(|_| internal("state lock poisoned"))?;
        for session_id in live_session_ids {
            // Tolerate a session that vanished on its own (PTY-exit dispatcher)
            // or was closed by another client; a force-removal must continue.
            if let Some(session) = state.sessions.remove(&session_id) {
                closed.push(session);
            }
        }
        let racing_ids = state
            .sessions
            .values()
            .filter(|session| session.session.parent == SessionParent::Worktree(worktree_id))
            .map(|session| session.session.id)
            .collect::<Vec<_>>();
        if !force && !racing_ids.is_empty() {
            return Err(ProtocolError::new(
                ErrorCode::LiveSessions,
                "worktree has live sessions; retry with force to kill them",
            ));
        }
        for session_id in racing_ids {
            if let Some(session) = state.sessions.remove(&session_id) {
                closed.push(session);
            }
        }
        // Hide the worktree from new OpenSession requests while the bounded git
        // removal runs without the global state mutex. Both the worktree and the
        // session store rows are deleted only after git succeeds (below) so a
        // failed removal can be restored.
        state.worktrees.remove(&worktree_id);
    }
    // Kill the PTYs (their handles were removed from the map above). This fires
    // the PTY-exit dispatcher, which deletes their store rows; the re-insert on
    // git failure runs after the settle below, so it lands after the dispatcher
    // has processed those exits.
    let mut closed_session_ids = Vec::new();
    let mut closed_sessions = Vec::new();
    for session in closed {
        let _ = session.pty.kill();
        closed_session_ids.push(session.session.id);
        closed_sessions.push(session.session);
    }
    if force && !closed_session_ids.is_empty() {
        thread::sleep(Duration::from_millis(500));
    }

    if let Err(err) = remove_git_worktree_bounded(
        &git_path,
        &project.root,
        &worktree.path,
        force,
        delete_branch.then_some(worktree.branch.as_str()),
    ) {
        let mut state = state.lock().map_err(|_| internal("state lock poisoned"))?;
        // Restore the in-memory worktree row so the GUI shows it again, and
        // re-insert the closed sessions' layout rows so the next daemon launch
        // revives them as fresh terminals. The PTY-exit dispatcher has by now
        // deleted those rows, so a plain insert is the expected path; tolerate a
        // surviving row (a slow dispatcher) by replacing it.
        state.worktrees.entry(worktree_id).or_insert(worktree);
        for session in &closed_sessions {
            let _ = state.store.delete_session(session.id);
            if let Err(insert_err) = state.store.insert_session(session) {
                eprintln!(
                    "hitch-daemon: failed to restore session after git removal failed: {}",
                    store_error(insert_err).message
                );
            }
        }
        return Err(err);
    }

    // Git removal succeeded: now delete the closed sessions' store rows. The
    // PTY-exit dispatcher most likely already deleted them, so tolerate a
    // missing row rather than treating it as an error.
    {
        let state = state.lock().map_err(|_| internal("state lock poisoned"))?;
        for session in &closed_sessions {
            let _ = state.store.delete_session(session.id);
        }
    }

    let post_remove_orphans = {
        let mut state = state.lock().map_err(|_| internal("state lock poisoned"))?;
        let racing_ids = state
            .sessions
            .values()
            .filter(|session| session.session.parent == SessionParent::Worktree(worktree_id))
            .map(|session| session.session.id)
            .collect::<Vec<_>>();
        state
            .store
            .delete_worktree(worktree_id)
            .map_err(store_error)?;
        state.worktrees.remove(&worktree_id);
        racing_ids
            .into_iter()
            .filter_map(|id| state.sessions.remove(&id).map(|session| (id, session)))
            .collect::<Vec<_>>()
    };

    for (session_id, session) in post_remove_orphans {
        let _ = session.pty.kill();
        closed_session_ids.push(session_id);
    }
    Ok(closed_session_ids)
}

fn open_session(
    state: &Arc<Mutex<DaemonState>>,
    parent: SessionParent,
    name: String,
    command: Option<Vec<String>>,
    cols: u16,
    rows: u16,
    pty_tx: &mpsc::Sender<PtyEvent>,
) -> Result<Session, ProtocolError> {
    if let SessionParent::Worktree(worktree_id) = parent {
        // Hook installation is best-effort: a malformed or unwritable agent
        // config (`.claude/settings.local.json`, `.codex/hooks.json`) must not
        // block launching a plain terminal. Agent-state tracking simply degrades
        // for this session until the config is fixed; reopening reinstalls hooks.
        if let Err(err) = install_agent_hooks_for_worktree_id(state, worktree_id) {
            eprintln!("hitch-daemon: {}", err.message);
        }
    }
    let cwd = session_parent_cwd(state, parent)?;
    let session = Session::new(name, parent, cwd.clone());
    // Spawn at the client's initial grid so the terminal doesn't visibly reflow
    // on its first fit. A `0` in either dimension means the client couldn't
    // measure a size yet, so fall back to the daemon default rather than ever
    // spawning a degenerate 0-sized PTY.
    let size = if cols == 0 || rows == 0 {
        TerminalSize::default()
    } else {
        TerminalSize::new(cols, rows)
    };
    let pty = ManagedPty::spawn(
        PtySpawnConfig::new(session.id, cwd)
            .command(command)
            .size(size),
        pty_tx.clone(),
    )
    .map_err(|err| ProtocolError::new(ErrorCode::PtyFailed, err.to_string()))?;

    let mut state = state.lock().map_err(|_| internal("state lock poisoned"))?;
    state.store.insert_session(&session).map_err(store_error)?;
    state.sessions.insert(
        session.id,
        DaemonSession {
            session: session.clone(),
            pty,
            restored_scrollback: Vec::new(),
            agent: None,
            agent_state: None,
            agent_detail: None,
            agent_report_requires_running: false,
            output_active: false,
            last_output_at: None,
        },
    );
    Ok(session)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SessionOpenedReplay {
    agent: Option<KnownAgent>,
    agent_state: Option<AgentState>,
    agent_detail: Option<String>,
    output_active: bool,
}

fn session_opened_replay(
    state: &Arc<Mutex<DaemonState>>,
    session_id: SessionId,
) -> Result<SessionOpenedReplay, ProtocolError> {
    let state = state.lock().map_err(|_| internal("state lock poisoned"))?;
    let daemon_session = state
        .sessions
        .get(&session_id)
        .ok_or_else(|| ProtocolError::new(ErrorCode::InvalidRequest, "session not found"))?;
    Ok(SessionOpenedReplay {
        agent: daemon_session.agent,
        agent_state: daemon_session.agent_state,
        agent_detail: daemon_session.agent_detail.clone(),
        output_active: daemon_session.output_active,
    })
}

fn remove_git_worktree_bounded(
    git_path: &Path,
    repo_root: &Path,
    worktree_path: &Path,
    force: bool,
    delete_branch: Option<&str>,
) -> Result<(), ProtocolError> {
    if force {
        remove_worktree_dir_after_forced_session_close(worktree_path)?;
        run_git_with_timeout(
            git_path,
            repo_root,
            [
                OsArg::Borrowed("worktree"),
                OsArg::Borrowed("prune"),
                OsArg::Borrowed("--expire=now"),
            ],
            Duration::from_secs(5),
        )?;
    } else {
        let args = [
            OsArg::Borrowed("worktree"),
            OsArg::Borrowed("remove"),
            // Pass the path as an OsString so a non-UTF-8 path on Unix reaches
            // git verbatim; a lossy String conversion would corrupt it and make
            // `git worktree remove` fail to find the worktree.
            OsArg::OwnedOs(worktree_path.as_os_str().to_owned()),
        ];
        match run_git_with_timeout(git_path, repo_root, args, Duration::from_secs(10)) {
            Ok(()) => {}
            Err(err)
                if !worktree_path.exists() && err.message.contains("is not a working tree") => {}
            Err(err) => return Err(err),
        }
    }

    if let Some(branch) = delete_branch {
        // Let `git branch -d` perform its own merged-safety check. It refuses to
        // delete an unmerged branch and surfaces a descriptive error, while also
        // honoring git's full safety semantics (merged into HEAD *or* its
        // upstream) — broader than a local `merge-base --is-ancestor HEAD`
        // preflight, which would reject deletions git itself allows.
        run_git_with_timeout(
            git_path,
            repo_root,
            [
                OsArg::Borrowed("branch"),
                OsArg::Borrowed("-d"),
                OsArg::Borrowed(branch),
            ],
            Duration::from_secs(5),
        )?;
    }
    Ok(())
}

fn remove_worktree_dir_after_forced_session_close(
    worktree_path: &Path,
) -> Result<(), ProtocolError> {
    let mut last_error = None;
    for attempt in 0..50 {
        match fs::remove_dir_all(worktree_path) {
            Ok(()) => return Ok(()),
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(err) if attempt < 49 => {
                last_error = Some(err);
                thread::sleep(Duration::from_millis(100));
            }
            Err(err) => {
                last_error = Some(err);
                break;
            }
        }
    }
    Err(ProtocolError::new(
        ErrorCode::GitFailed,
        format!(
            "failed to remove worktree directory {} after closing sessions: {}",
            worktree_path.display(),
            last_error
                .map(|err| err.to_string())
                .unwrap_or_else(|| "timed out".to_string())
        ),
    ))
}

enum OsArg<'a> {
    Borrowed(&'a str),
    OwnedOs(OsString),
}

/// Bounded wait for a git reader thread to reach EOF after the child exited.
/// Matches `hitch-git::READER_DRAIN_GRACE`; the child is already reaped here, so
/// a still-parked reader is detached with whatever git wrote.
const GIT_READER_DRAIN_GRACE: Duration = Duration::from_millis(500);

fn run_git_with_timeout<'a, I>(
    git_path: &Path,
    repo_root: &Path,
    args: I,
    timeout: Duration,
) -> Result<(), ProtocolError>
where
    I: IntoIterator<Item = OsArg<'a>>,
{
    let mut command = Command::new(git_path);
    command.current_dir(repo_root).stdin(Stdio::null());
    // This git invocation does not go through `ProcessTree::spawn`, so suppress
    // the console window explicitly. See `hitch_process::configure_windowless`.
    hitch_process::configure_windowless(&mut command);
    for arg in args {
        match arg {
            OsArg::Borrowed(arg) => {
                command.arg(arg);
            }
            OsArg::OwnedOs(arg) => {
                command.arg(arg);
            }
        };
    }
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| ProtocolError::new(ErrorCode::GitFailed, err.to_string()))?;
    // Drain stdout/stderr on reader threads while we poll. Without concurrent
    // draining, git that writes more than the OS pipe buffer (~64KB) blocks on a
    // full pipe and never exits, so the poll loop below would spuriously kill it
    // at the deadline. `PipeReader` is the shared primitive used by `hitch-git`
    // and `drafts.rs` for exactly this. We never read stdout's content (only
    // stderr feeds the error message), but the reader must stay bound for the
    // whole function so its thread keeps draining the pipe.
    let _stdout_reader = child.stdout.take().map(PipeReader::spawn);
    let stderr_reader = child.stderr.take().map(PipeReader::spawn);
    let started = std::time::Instant::now();
    loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|err| ProtocolError::new(ErrorCode::GitFailed, err.to_string()))?
        {
            if status.success() {
                return Ok(());
            }
            // The child exited on its own, so its write ends are closed and the
            // readers reach EOF. Bounded-drain to collect stderr for the message
            // without blocking on a stuck reader.
            let stderr_bytes = stderr_reader
                .map(drain_git_reader_bounded)
                .unwrap_or_default();
            let stderr = String::from_utf8_lossy(&stderr_bytes);
            return Err(ProtocolError::new(
                ErrorCode::GitFailed,
                format!("git failed: {stderr}"),
            ));
        }
        if started.elapsed() >= timeout {
            // Killing the child closes the captured write ends, so the reader
            // threads reach EOF and finish; drop them implicitly without waiting.
            let _ = child.kill();
            let _ = child.wait();
            return Err(ProtocolError::new(
                ErrorCode::GitFailed,
                "git worktree remove timed out waiting for the worktree path to become removable",
            ));
        }
        thread::sleep(Duration::from_millis(25));
    }
}

/// Collect a finished git reader's output, collapsing the drained and
/// timed-out outcomes to the bytes read (the daemon uses whatever git already
/// wrote for the error message). Mirrors `hitch-git`'s `drain_pipe_reader_bounded`.
fn drain_git_reader_bounded(reader: PipeReader) -> Vec<u8> {
    reader
        .drain_bounded(GIT_READER_DRAIN_GRACE)
        .map(DrainOutcome::into_inner)
        .unwrap_or_default()
}

fn close_session(
    state: &Arc<Mutex<DaemonState>>,
    session_id: SessionId,
    kill_process: bool,
) -> Result<(), ProtocolError> {
    let removed = {
        let mut state = state.lock().map_err(|_| internal("state lock poisoned"))?;
        let removed = state
            .sessions
            .remove(&session_id)
            .ok_or_else(|| ProtocolError::new(ErrorCode::NotFound, "session not found"))?;
        state
            .store
            .delete_session(session_id)
            .map_err(store_error)?;
        removed
    };
    if kill_process {
        let _ = removed.pty.kill();
    }
    Ok(())
}

fn rename_session(
    state: &Arc<Mutex<DaemonState>>,
    session_id: SessionId,
    name: String,
) -> Result<(), ProtocolError> {
    let mut state = state.lock().map_err(|_| internal("state lock poisoned"))?;
    let updated = {
        let session = state
            .sessions
            .get_mut(&session_id)
            .ok_or_else(|| ProtocolError::new(ErrorCode::NotFound, "session not found"))?;
        session.session.name = name;
        session.session.clone()
    };
    state.store.update_session(&updated).map_err(store_error)?;
    if let Some(session) = state.sessions.get_mut(&session_id) {
        session.session = updated;
    }
    Ok(())
}

fn session_parent_cwd(
    state: &Arc<Mutex<DaemonState>>,
    parent: SessionParent,
) -> Result<PathBuf, ProtocolError> {
    let state = state.lock().map_err(|_| internal("state lock poisoned"))?;
    let cwd = match parent {
        SessionParent::Worktree(id) => state
            .worktrees
            .get(&id)
            .map(|worktree| worktree.path.clone())
            .ok_or_else(|| ProtocolError::new(ErrorCode::NotFound, "worktree not found"))?,
        SessionParent::Project(id) => state
            .projects
            .get(&id)
            .map(|project| project.root.clone())
            .ok_or_else(|| ProtocolError::new(ErrorCode::NotFound, "project not found"))?,
    };
    if cwd.is_dir() {
        Ok(cwd)
    } else {
        Err(ProtocolError::new(
            ErrorCode::InvalidRequest,
            format!("session cwd is not a directory: {}", cwd.display()),
        ))
    }
}

fn git_status(
    state: &Arc<Mutex<DaemonState>>,
    worktree_id: WorktreeId,
) -> Result<GitStatus, ProtocolError> {
    let worktree = {
        let state = state.lock().map_err(|_| internal("state lock poisoned"))?;
        state
            .worktrees
            .get(&worktree_id)
            .cloned()
            .ok_or_else(|| ProtocolError::new(ErrorCode::NotFound, "worktree not found"))?
    };
    let (worktree, branch_changed) = refresh_worktree_branch_from_disk(state, worktree)?;
    let repo = GitRepository::discover(&worktree.path).map_err(git_error)?;
    let summary = repo.status().map_err(git_error)?;
    let (ahead, behind) = repo.ahead_behind().unwrap_or((0, 0));
    if branch_changed {
        broadcast_event(
            state,
            Event::WorktreeUpdated {
                worktree: worktree.clone(),
            },
        )?;
    }
    Ok(GitStatus {
        worktree_id,
        branch: worktree.branch,
        dirty: summary.dirty,
        ahead,
        behind,
        additions: summary.additions.min(u32::MAX as usize) as u32,
        deletions: summary.deletions.min(u32::MAX as usize) as u32,
        files: summary.entries.iter().map(status_entry_to_proto).collect(),
    })
}

fn pr_status(
    state: &Arc<Mutex<DaemonState>>,
    worktree_id: WorktreeId,
    control: &JobControl,
) -> Result<Option<PrInfo>, ProtocolError> {
    let (git, worktree) = refreshed_worktree_context(state, worktree_id)?;
    let pr = git
        .pr_status_with_control(&worktree.path, control)
        .map_err(git_error)?;
    Ok(pr.map(|pr| PrInfo {
        number: pr.number,
        url: pr.url,
        state: pr.state,
        draft: pr.draft,
    }))
}

fn project_pr_statuses(
    state: &Arc<Mutex<DaemonState>>,
    project_id: ProjectId,
    control: &JobControl,
) -> Result<Vec<WorktreePr>, ProtocolError> {
    let worktrees = list_worktrees(state, project_id)?;
    if worktrees.is_empty() {
        return Ok(Vec::new());
    }
    // All worktrees of a project share the repo + remote, so one `gh pr list`
    // covers them all. Prefer the main worktree as a stable cwd.
    let repo_path = worktrees
        .iter()
        .find(|w| w.is_main)
        .unwrap_or(&worktrees[0])
        .path
        .clone();
    let git = {
        let state = state.lock().map_err(|_| internal("state lock poisoned"))?;
        state.git.clone()
    };
    // Scope the lookup to our worktree branches so a long PR history can't
    // truncate them out of the result. The git layer exact-matches headRefName
    // after GitHub's prefix search returns.
    let branches: Vec<String> = worktrees.iter().map(|w| w.branch.clone()).collect();
    let prs = git
        .pr_list_for_branches_with_control(&repo_path, &branches, control)
        .map_err(git_error)?;
    Ok(worktrees
        .into_iter()
        .map(|worktree| WorktreePr {
            pr: best_pr_for_branch(&prs, &worktree.branch),
            worktree_id: worktree.id,
        })
        .collect())
}

/// Pick the PR that best represents a branch when the batched lookup returned
/// more than one for it: an open PR wins over a closed/merged one, and among
/// equals the highest number (most recent) wins. Returns the proto `PrInfo`.
fn best_pr_for_branch(prs: &[(String, hitch_git::PrInfo)], branch: &str) -> Option<PrInfo> {
    prs.iter()
        .filter(|(head, _)| head == branch)
        .max_by_key(|(_, pr)| (pr.state.eq_ignore_ascii_case("OPEN"), pr.number))
        .map(|(_, pr)| PrInfo {
            number: pr.number,
            url: pr.url.clone(),
            state: pr.state.clone(),
            draft: pr.draft,
        })
}

fn git_diff(
    state: &Arc<Mutex<DaemonState>>,
    worktree_id: WorktreeId,
    path: PathBuf,
) -> Result<FileDiff, ProtocolError> {
    let worktree = {
        let state = state.lock().map_err(|_| internal("state lock poisoned"))?;
        state
            .worktrees
            .get(&worktree_id)
            .cloned()
            .ok_or_else(|| ProtocolError::new(ErrorCode::NotFound, "worktree not found"))?
    };
    let repo = GitRepository::discover(&worktree.path).map_err(git_error)?;
    let diff_path = diff_path_for_worktree(&worktree.path, &path);
    let mut diff = repo
        .diff_file(&diff_path, DiffTarget::Worktree)
        .map_err(git_error)?;
    if diff.is_empty() {
        diff = repo
            .diff_file(&diff_path, DiffTarget::Staged)
            .map_err(git_error)?;
    }
    Ok(FileDiff {
        worktree_id,
        path,
        diff,
    })
}

fn diff_path_for_worktree<'a>(worktree_path: &Path, path: &'a Path) -> std::borrow::Cow<'a, Path> {
    if path.is_absolute() {
        if let Ok(relative) = path.strip_prefix(worktree_path) {
            return std::borrow::Cow::Owned(relative.to_path_buf());
        }
        if let (Ok(root), Ok(file)) = (worktree_path.canonicalize(), path.canonicalize()) {
            if let Ok(relative) = file.strip_prefix(root) {
                return std::borrow::Cow::Owned(relative.to_path_buf());
            }
        }
    }
    std::borrow::Cow::Borrowed(path)
}

fn list_draft_models(
    state: &Arc<Mutex<DaemonState>>,
    provider: DraftProvider,
    settings: Option<DraftGenerationSettings>,
    cancel: Option<&JobControl>,
) -> Result<Vec<String>, ProtocolError> {
    let config = draft_provider_config(state)?;
    drafts::list_models(&config, provider, settings, cancel)
}

fn generate_commit_draft(
    state: &Arc<Mutex<DaemonState>>,
    worktree_id: WorktreeId,
    settings: Option<DraftGenerationSettings>,
    cancel: Option<&JobControl>,
) -> Result<CommitDraft, ProtocolError> {
    let worktree = refreshed_worktree_context(state, worktree_id)?.1;
    let provider = draft_provider_config(state)?.with_settings(settings);
    let repo = GitRepository::discover(&worktree.path).map_err(git_error)?;
    let summary = repo.status().map_err(git_error)?;
    let staged_paths = summary
        .entries
        .iter()
        .filter(|entry| index_is_staged(entry.index))
        .map(|entry| entry.path.clone())
        .collect::<Vec<_>>();
    if staged_paths.is_empty() {
        return Err(ProtocolError::new(
            ErrorCode::InvalidRequest,
            "stage files before generating a commit draft",
        ));
    }

    // Compose from the staged side only; unstaged worktree edits are ignored.
    let staged_patch = staged_diff(&worktree.path).map_err(git_error)?;
    drafts::generate_commit_draft(
        &provider,
        CommitDraftInput {
            worktree_path: worktree.path,
            staged_paths,
            staged_patch,
        },
        cancel,
    )
}

fn generate_pull_request_draft(
    state: &Arc<Mutex<DaemonState>>,
    worktree_id: WorktreeId,
    base: Option<String>,
    settings: Option<DraftGenerationSettings>,
    cancel: Option<&JobControl>,
) -> Result<PullRequestDraft, ProtocolError> {
    let worktree = refreshed_worktree_context(state, worktree_id)?.1;
    let provider = draft_provider_config(state)?.with_settings(settings);
    let repo = GitRepository::discover(&worktree.path).map_err(git_error)?;
    let base = base
        .map(|base| base.trim().to_string())
        .filter(|base| !base.is_empty())
        .ok_or_else(|| {
            ProtocolError::new(
                ErrorCode::InvalidRequest,
                "enter a base branch before generating a PR draft",
            )
        })?;
    if base == worktree.branch {
        return Err(ProtocolError::new(
            ErrorCode::InvalidRequest,
            "choose a base branch different from the current branch",
        ));
    }

    // One pass over the repo for commits, changed paths, and patch text rather
    // than three re-discoveries that each rebuild the branch diff.
    let comparison = repo.branch_comparison(&base, 25).map_err(git_error)?;
    let commit_summaries = comparison
        .commits
        .iter()
        .filter_map(|commit| commit.summary.clone())
        .collect::<Vec<_>>();

    drafts::generate_pull_request_draft(
        &provider,
        PullRequestDraftInput {
            worktree_path: worktree.path,
            branch: worktree.branch,
            base,
            commits: commit_summaries,
            changed_paths: comparison.changed_paths,
            diff: comparison.diff,
        },
        cancel,
    )
}

fn draft_provider_config(
    state: &Arc<Mutex<DaemonState>>,
) -> Result<DraftProviderConfig, ProtocolError> {
    let state = state.lock().map_err(|_| internal("state lock poisoned"))?;
    Ok(state.config.draft_provider.clone())
}

fn git_context(
    state: &Arc<Mutex<DaemonState>>,
    worktree_id: WorktreeId,
) -> Result<(GitClient, PathBuf), ProtocolError> {
    let state = state.lock().map_err(|_| internal("state lock poisoned"))?;
    let worktree = state
        .worktrees
        .get(&worktree_id)
        .ok_or_else(|| ProtocolError::new(ErrorCode::NotFound, "worktree not found"))?;
    Ok((state.git.clone(), worktree.path.clone()))
}

fn refreshed_worktree_context(
    state: &Arc<Mutex<DaemonState>>,
    worktree_id: WorktreeId,
) -> Result<(GitClient, Worktree), ProtocolError> {
    let (git, worktree) = {
        let state = state.lock().map_err(|_| internal("state lock poisoned"))?;
        let worktree = state
            .worktrees
            .get(&worktree_id)
            .cloned()
            .ok_or_else(|| ProtocolError::new(ErrorCode::NotFound, "worktree not found"))?;
        (state.git.clone(), worktree)
    };
    let (worktree, branch_changed) = refresh_worktree_branch_from_disk(state, worktree)?;
    if branch_changed {
        broadcast_event(
            state,
            Event::WorktreeUpdated {
                worktree: worktree.clone(),
            },
        )?;
    }
    Ok((git, worktree))
}

fn broadcast_dirty(
    state: &Arc<Mutex<DaemonState>>,
    worktree_id: WorktreeId,
) -> Result<(), ProtocolError> {
    let path = {
        let state = state.lock().map_err(|_| internal("state lock poisoned"))?;
        state
            .worktrees
            .get(&worktree_id)
            .map(|worktree| worktree.path.clone())
            .ok_or_else(|| ProtocolError::new(ErrorCode::NotFound, "worktree not found"))?
    };
    let dirty = GitRepository::discover(&path)
        .map_err(git_error)?
        .is_dirty()
        .map_err(git_error)?;
    broadcast_event(state, Event::WorktreeDirty { worktree_id, dirty })
}

/// True when the index side reflects a genuine staged change. A conflicted file
/// has an index state of [`FileState::Conflicted`], but an unresolved conflict
/// is not a clean staged change, so it is excluded here.
fn index_is_staged(state: FileState) -> bool {
    !matches!(state, FileState::Unmodified | FileState::Conflicted)
}

fn status_entry_to_proto(entry: &StatusEntry) -> ChangedFile {
    let staged = index_is_staged(entry.index);
    let state = if staged {
        entry.index
    } else {
        entry.working_tree
    };
    ChangedFile {
        path: entry.path.clone(),
        status: match state {
            FileState::New if !staged => FileStatus::Untracked,
            FileState::New => FileStatus::Added,
            FileState::Modified | FileState::Typechange => FileStatus::Modified,
            FileState::Deleted => FileStatus::Deleted,
            FileState::Renamed => FileStatus::Renamed,
            FileState::Conflicted => FileStatus::Conflicted,
            FileState::Unmodified => FileStatus::Modified,
        },
        staged,
    }
}

/// Poll git worktrees once a second and broadcast dirty changes caused outside
/// Hitch's own git buttons (editors, shells, agents). This keeps project-tree
/// badges live without asking the GUI to full-status every worktree.
fn spawn_dirty_poller(state: Arc<Mutex<DaemonState>>, shutdown: Arc<AtomicBool>) {
    thread::Builder::new()
        .name("hitch-dirty-poll".into())
        .spawn(move || {
            let mut last: HashMap<WorktreeId, bool> = HashMap::new();
            while !shutdown.load(Ordering::SeqCst) {
                let worktrees: Vec<(WorktreeId, PathBuf)> = match state.lock() {
                    Ok(state) => state
                        .worktrees
                        .iter()
                        .map(|(id, worktree)| (*id, worktree.path.clone()))
                        .collect(),
                    Err(_) => Vec::new(),
                };
                last.retain(|id, _| {
                    worktrees
                        .iter()
                        .any(|(worktree_id, _)| worktree_id == id)
                });
                for (worktree_id, path) in worktrees {
                    let Ok(dirty) = GitRepository::discover(&path).and_then(|repo| repo.is_dirty())
                    else {
                        continue;
                    };
                    if matches!(last.insert(worktree_id, dirty), Some(previous) if previous != dirty) {
                        let _ = broadcast_event(
                            &state,
                            Event::WorktreeDirty { worktree_id, dirty },
                        );
                    }
                }
                thread::sleep(Duration::from_secs(1));
            }
        })
        .expect("failed to spawn dirty poller thread");
}

/// Poll each live session's foreground command once a second and broadcast
/// a SessionCommand event whenever it changes. Broadcasting only on change
/// keeps idle terminals quiet. Agent-state exit detection is deliberately
/// conservative: tools spawned by an agent can become the foreground process,
/// so only returning to an interactive shell is treated as "agent gone".
///
/// Unix-only: ManagedPty::foreground_command() always returns `None` on Windows
/// (ConPTY exposes no foreground process group — see hitch-pty and ADR 0011's
/// "Windows note"), so this poller would be a per-second no-op there and is not
/// spawned on non-Unix platforms.
#[cfg(unix)]
fn spawn_command_poller(state: Arc<Mutex<DaemonState>>, shutdown: Arc<AtomicBool>) {
    thread::Builder::new()
        .name("hitch-cmd-poll".into())
        .spawn(move || {
            let mut last: HashMap<SessionId, Option<String>> = HashMap::new();
            while !shutdown.load(Ordering::SeqCst) {
                let ptys: Vec<(SessionId, Arc<ManagedPty>)> = match state.lock() {
                    Ok(state) => state
                        .sessions
                        .iter()
                        .map(|(id, session)| (*id, Arc::clone(&session.pty)))
                        .collect(),
                    Err(_) => Vec::new(),
                };
                // Forget commands for sessions that have closed.
                last.retain(|id, _| ptys.iter().any(|(pid, _)| pid == id));
                for (id, pty) in ptys {
                    let command = pty.foreground_command();
                    if last.get(&id) != Some(&command) {
                        last.insert(id, command.clone());
                        let _ = broadcast_event(
                            &state,
                            Event::SessionCommand {
                                session_id: id,
                                command: command.clone(),
                            },
                        );
                    }
                    if let Some(event) = clear_stale_agent_state(&state, id, command.as_deref()) {
                        let _ = broadcast_agent_state_event(
                            &state,
                            Event::AgentState {
                                session_id: Some(event.session_id),
                                worktree_id: event.worktree_id,
                                agent: event.agent,
                                state: event.state,
                                detail: event.detail,
                            },
                        );
                    }
                }
                thread::sleep(Duration::from_millis(1000));
            }
        })
        .expect("failed to spawn command poller thread");
}

/// Poll for output-activity falling edges (ADR 0011 amendment 2026-06-05). The
/// rising edge is emitted inline on the dispatcher thread when a frame arrives;
/// the falling edge has no triggering event, so this thread checks once per
/// [`OUTPUT_ACTIVE_POLL_INTERVAL`] whether any active session has been quiet for
/// [`OUTPUT_ACTIVE_QUIET`] and broadcasts its `active: false` transition. The
/// daemon only ever watches WHETHER frames arrived (the timestamp), never their
/// content — ADR 0011's no-text-inference rule. Spawned on all platforms: it
/// reads only the in-memory gate, with no dependence on `foreground_command()`.
fn spawn_output_activity_poller(state: Arc<Mutex<DaemonState>>, shutdown: Arc<AtomicBool>) {
    thread::Builder::new()
        .name("hitch-output-active-poll".into())
        .spawn(move || {
            while !shutdown.load(Ordering::SeqCst) {
                let edges = match state.lock() {
                    Ok(mut state) => {
                        collect_output_quiet_edges(&mut state, Instant::now(), OUTPUT_ACTIVE_QUIET)
                    }
                    Err(_) => Vec::new(),
                };
                for edge in edges {
                    let _ = broadcast_event(&state, edge);
                }
                thread::sleep(OUTPUT_ACTIVE_POLL_INTERVAL);
            }
        })
        .expect("failed to spawn output-activity poller thread");
}

/// Forward PTY reader events into the dispatcher's single ordered channel as
/// `DispatchMsg::Pty`. The `ManagedPty` contract emits `PtyEvent`, but replay
/// requests must travel the SAME queue as output so the dispatcher can serialize
/// them; this thin FIFO bridge merges the two without reordering output.
fn spawn_pty_bridge(rx: mpsc::Receiver<PtyEvent>, dispatch_tx: mpsc::Sender<DispatchMsg>) {
    thread::Builder::new()
        .name("hitch-pty-bridge".into())
        .spawn(move || {
            while let Ok(event) = rx.recv() {
                if dispatch_tx.send(DispatchMsg::Pty(event)).is_err() {
                    break;
                }
            }
        })
        .expect("failed to spawn PTY bridge thread");
}

fn spawn_pty_dispatcher(
    state: Arc<Mutex<DaemonState>>,
    rx: mpsc::Receiver<DispatchMsg>,
    shutdown: Arc<AtomicBool>,
) {
    thread::Builder::new()
        .name("hitch-pty-dispatch".into())
        .spawn(move || {
            // Everything below runs on this single thread, which is the whole
            // point: appending to the broadcast log, opening a client's gate
            // during replay, and skipping non-live clients on broadcast all see a
            // consistent, totally-ordered view (see `OutputBroadcaster`).
            while let Ok(message) = rx.recv() {
                match message {
                    DispatchMsg::Pty(PtyEvent::Output { session_id, bytes }) => {
                        // Record into the authoritative log BEFORE broadcasting,
                        // so the log always equals "what has been broadcast so
                        // far" — a replay enqueued after this point will include
                        // these bytes in its snapshot rather than racing them.
                        record_and_broadcast_output(&state, session_id, &bytes);
                    }
                    DispatchMsg::Pty(PtyEvent::Exited {
                        session_id,
                        exit_code,
                    }) => {
                        // During a graceful "Quit Hitch", kill_all_sessions kills the
                        // PTYs, which fires Exited here. Keep those sessions in the
                        // store so the next launch restores the layout as fresh
                        // terminals (ADR 0003); only forget sessions whose process
                        // exited while Hitch was running.
                        let shutting_down = shutdown.load(Ordering::SeqCst);
                        if let Ok(mut state) = state.lock() {
                            state.sessions.remove(&session_id);
                            state.broadcaster.forget_session(session_id);
                            if !shutting_down {
                                let _ = state.store.delete_session(session_id);
                            }
                        }
                        let _ = broadcast_event(
                            &state,
                            Event::SessionClosed {
                                session_id,
                                exit_code,
                            },
                        );
                    }
                    DispatchMsg::ReplayToClient { client_id } => {
                        // Replay each session's snapshot from the authoritative
                        // log, then open the client's output gate — both on this
                        // thread, so every subsequent Output is broadcast to the
                        // now-live client strictly after its snapshot.
                        let _ = replay_sessions_to_client(&state, client_id);
                    }
                }
            }
        })
        .expect("failed to spawn PTY dispatch thread");
}

/// Append output to the session's authoritative broadcast log, then write it to
/// every client whose output gate is open. Runs only on the dispatcher thread.
/// Holds the state lock once to record, then broadcasts; clients still
/// mid-replay (gate closed) are skipped by [`broadcast_session_output`].
fn record_and_broadcast_output(
    state: &Arc<Mutex<DaemonState>>,
    session_id: SessionId,
    bytes: &[u8],
) {
    let rising_edge = if let Ok(mut state) = state.lock() {
        // The log holds live bytes only; restored scrollback is prepended in
        // `replay_snapshot` and does not need to be passed for every frame.
        state.broadcaster.record_output(session_id, bytes);
        // Output-activity gate (ADR 0011 amendment 2026-06-05): stamp the
        // last-output instant on EVERY frame (cheap, no broadcast), but only emit
        // a rising edge the first time a quiet session goes active. While already
        // active we never broadcast per frame, so a busy session does not spam.
        mark_output_active(&mut state, session_id, Instant::now())
    } else {
        None
    };
    let _ = broadcast_session_output(state, session_id, bytes);
    if let Some(edge) = rising_edge {
        let _ = broadcast_event(state, edge);
    }
}

/// Record an output frame's arrival on a session's activity gate, returning a
/// rising-edge [`Event::OutputActive`] (`active: true`) only when the session was
/// previously inactive. Always refreshes `last_output_at` so the poller's falling
/// edge measures from the most recent frame. Returns `None` when the session is
/// already active (no per-frame broadcast) or unknown. Runs on the dispatcher
/// thread under the state lock.
fn mark_output_active(
    state: &mut DaemonState,
    session_id: SessionId,
    now: Instant,
) -> Option<Event> {
    let daemon_session = state.sessions.get_mut(&session_id)?;
    daemon_session.last_output_at = Some(now);
    if daemon_session.output_active {
        return None;
    }
    daemon_session.output_active = true;
    Some(Event::OutputActive {
        session_id,
        worktree_id: session_worktree_id(&daemon_session.session),
        active: true,
    })
}

/// Scan every session for a falling edge: a session that is currently marked
/// active but whose last output frame is older than [`OUTPUT_ACTIVE_QUIET`] flips
/// to inactive. Returns one `active: false` [`Event::OutputActive`] per session
/// that just went quiet (the falling edge). A session with no output yet
/// (`last_output_at == None`) is never active, so it is skipped. Pure over the
/// passed `now` so tests can drive the clock deterministically.
fn collect_output_quiet_edges(
    state: &mut DaemonState,
    now: Instant,
    quiet: Duration,
) -> Vec<Event> {
    let mut edges = Vec::new();
    for daemon_session in state.sessions.values_mut() {
        if !daemon_session.output_active {
            continue;
        }
        let quiet_for = daemon_session
            .last_output_at
            .map(|last| now.saturating_duration_since(last));
        if matches!(quiet_for, Some(elapsed) if elapsed >= quiet) {
            daemon_session.output_active = false;
            edges.push(Event::OutputActive {
                session_id: daemon_session.session.id,
                worktree_id: session_worktree_id(&daemon_session.session),
                active: false,
            });
        }
    }
    edges
}

fn read_control_line<R: BufRead>(reader: &mut R) -> io::Result<Option<Vec<u8>>> {
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
    Ok(Some(line))
}

fn request_id_from_control_line(line: &[u8]) -> Option<u64> {
    let value: serde_json::Value = serde_json::from_slice(line).ok()?;
    if value.get("kind")?.as_str()? != "request" {
        return None;
    }
    value.get("id")?.as_u64()
}

fn read_pty_payload<R: Read>(reader: &mut R) -> Result<Vec<u8>, ProtocolError> {
    let mut prefix = [0_u8; 4];
    reader
        .read_exact(&mut prefix)
        .map_err(|err| ProtocolError::new(ErrorCode::InvalidRequest, err.to_string()))?;
    let len = u32::from_be_bytes(prefix) as usize;
    if len > MAX_PTY_FRAME_LEN {
        return Err(ProtocolError::new(
            ErrorCode::InvalidRequest,
            format!("PTY input frame length {len} exceeds max {MAX_PTY_FRAME_LEN}"),
        ));
    }
    let mut payload = vec![0_u8; len];
    reader
        .read_exact(&mut payload)
        .map_err(|err| ProtocolError::new(ErrorCode::InvalidRequest, err.to_string()))?;
    Ok(payload)
}

fn find_pty(
    state: &Arc<Mutex<DaemonState>>,
    session_id: SessionId,
) -> Result<Arc<ManagedPty>, ProtocolError> {
    let state = state.lock().map_err(|_| internal("state lock poisoned"))?;
    state
        .sessions
        .get(&session_id)
        .map(|session| Arc::clone(&session.pty))
        .ok_or_else(|| ProtocolError::new(ErrorCode::NotFound, "session not found"))
}

struct AgentStateBroadcast {
    session_id: SessionId,
    worktree_id: Option<WorktreeId>,
    /// `None` on the identity clear (exit-to-`None`); `Some` on state reports
    /// and identity announces. Mirrors `Event::AgentState.agent` — a null
    /// *state* alone never clears identity (the pre-prompt announce carries
    /// `state: None` too, see ADR 0011 amendment 2026-06-05).
    agent: Option<KnownAgent>,
    state: Option<AgentState>,
    detail: Option<String>,
}

fn store_agent_report(
    state: &Arc<Mutex<DaemonState>>,
    agent: KnownAgent,
    agent_state: Option<AgentState>,
    session_id: Option<SessionId>,
    cwd: Option<&Path>,
    detail: Option<String>,
) -> Result<Option<AgentStateBroadcast>, ProtocolError> {
    let mut state = state.lock().map_err(|_| internal("state lock poisoned"))?;
    let Some(session_id) = session_id else {
        eprintln!(
            "hitch-daemon: dropping agent state report: agent={agent:?} cwd={} reason=missing-session-id",
            cwd.map(path_display).unwrap_or("<none>")
        );
        return Ok(None);
    };
    let Some(daemon_session) = state.sessions.get_mut(&session_id) else {
        eprintln!(
            "hitch-daemon: dropping agent state report: agent={agent:?} cwd={} reason=unknown-session-id",
            cwd.map(path_display).unwrap_or("<none>")
        );
        return Ok(None);
    };

    // The late-arrival guard drops stale *state* reports (waiting/error from a
    // dying agent's queued hooks) until a fresh `running` arrives. Clears are
    // exempt: a clear is idempotent, and for a never-prompted agent (identity
    // announced, no state, guard still closed from the previous run's exit) the
    // SessionEnd clear is the only thing that reverts the Session mark to shell.
    if daemon_session.agent_report_requires_running
        && agent_state.is_some()
        && !matches!(agent_state, Some(AgentState::Running))
    {
        eprintln!(
            "hitch-daemon: dropping late agent state report: agent={agent:?} session_id={session_id} reason=awaiting-running"
        );
        return Ok(None);
    }

    let worktree_id = session_worktree_id(&daemon_session.session);
    match agent_state {
        Some(state) => {
            // Transition precedence (ADR 0011 amendment 2026-06-05): a `waiting`
            // report must never downgrade `error`. After `StopFailure` the agent
            // is idle at its prompt, so the ~60s idle-prompt heal
            // (`Notification[idle_prompt] → waiting`) — and any `Stop → waiting`
            // that follows a failed turn — would otherwise silently blank an
            // unseen failure. `error` clears only via a `running` report
            // (UserPromptSubmit → running) or exit-to-`None`. All other
            // transitions behave as before. This is a precedence rule on the
            // daemon-owned value, not text inference.
            if matches!(state, AgentState::Waiting)
                && matches!(daemon_session.agent_state, Some(AgentState::Error))
            {
                eprintln!(
                    "hitch-daemon: ignoring waiting report over error: agent={agent:?} session_id={session_id} reason=error-outranks-idle-heal"
                );
                return Ok(None);
            }
            daemon_session.agent = Some(agent);
            daemon_session.agent_state = Some(state);
            daemon_session.agent_detail = detail.clone();
            daemon_session.agent_report_requires_running = false;
            Ok(Some(AgentStateBroadcast {
                session_id,
                worktree_id,
                agent: Some(agent),
                state: Some(state),
                detail,
            }))
        }
        None => {
            // Idempotent: a clear over an already-cleared session (the dirty-exit
            // backstop raced a late SessionEnd) changes nothing — no broadcast,
            // and the guard is left untouched.
            if daemon_session.agent.is_none() && daemon_session.agent_state.is_none() {
                return Ok(None);
            }
            daemon_session.agent = None;
            daemon_session.agent_state = None;
            daemon_session.agent_detail = None;
            daemon_session.agent_report_requires_running = true;
            // Exit-to-`None`: broadcast `agent: None` so clients clear the
            // identity too — the null state alone must not be the signal (an
            // identity announce also broadcasts a null pre-prompt state).
            Ok(Some(AgentStateBroadcast {
                session_id,
                worktree_id,
                agent: None,
                state: None,
                detail: None,
            }))
        }
    }
}

/// Store an **identity-only announce** (ADR 0011 amendment 2026-06-05): the
/// agent's `SessionStart` declares *which* agent now runs in a session so the
/// Session mark can render before the first prompt. Identity is **not** state:
///
/// - It records `daemon_session.agent` and **never** touches `agent_state`,
///   `agent_detail`, or the late-arrival guard — a fresh, never-prompted agent
///   stays at no-state.
/// - It **bypasses** the late-arrival guard (`agent_report_requires_running`):
///   that guard governs *state* reports only; an announce always passes.
/// - Exit-to-`None` (a `ReportAgentState` with `state: None`) clears identity
///   along with state, reverting the mark to shell — handled in
///   [`store_agent_report`], not here.
///
/// Returns the stored identity if it changed (so the caller can propagate it to
/// attached clients the same way state reports do), or `None` when the report
/// could not resolve to a session or the identity was already current.
fn store_agent_announce(
    state: &Arc<Mutex<DaemonState>>,
    agent: KnownAgent,
    session_id: Option<SessionId>,
    cwd: Option<&Path>,
) -> Result<Option<AgentStateBroadcast>, ProtocolError> {
    let mut state = state.lock().map_err(|_| internal("state lock poisoned"))?;
    let Some(session_id) = session_id else {
        eprintln!(
            "hitch-daemon: dropping agent announce: agent={agent:?} cwd={} reason=missing-session-id",
            cwd.map(path_display).unwrap_or("<none>")
        );
        return Ok(None);
    };
    let Some(daemon_session) = state.sessions.get_mut(&session_id) else {
        eprintln!(
            "hitch-daemon: dropping agent announce: agent={agent:?} cwd={} reason=unknown-session-id",
            cwd.map(path_display).unwrap_or("<none>")
        );
        return Ok(None);
    };

    // Identity only: do not touch agent_state / agent_detail / the late-arrival
    // guard. If the identity is already current, nothing changed.
    if daemon_session.agent == Some(agent) {
        return Ok(None);
    }
    let worktree_id = session_worktree_id(&daemon_session.session);
    daemon_session.agent = Some(agent);
    Ok(Some(AgentStateBroadcast {
        session_id,
        worktree_id,
        agent: Some(agent),
        // The announce carries no state; broadcast the session's *current*
        // stored state (typically `None` pre-prompt) so an attached client
        // learns the new identity without inventing a state. The client keys
        // the identity clear on `agent: None`, never on this null state.
        state: daemon_session.agent_state,
        detail: daemon_session.agent_detail.clone(),
    }))
}

// Only the Unix command poller calls this (the ADR 0011 dirty-exit backstop).
// On Windows foreground_command() is always `None`, so the poller is not spawned.
#[cfg(unix)]
fn clear_stale_agent_state(
    state: &Arc<Mutex<DaemonState>>,
    session_id: SessionId,
    command: Option<&str>,
) -> Option<AgentStateBroadcast> {
    let mut state = state.lock().ok()?;
    let daemon_session = state.sessions.get_mut(&session_id)?;
    // Identity alone is enough to need the backstop: a never-prompted agent
    // (announced, no Agent State) that dies without SessionEnd would otherwise
    // hold the Session mark forever.
    let agent = daemon_session.agent?;
    let Some(command) = command else {
        return None;
    };
    if !foreground_command_is_shell(command) || agent_command_matches(agent, command) {
        return None;
    }
    let worktree_id = session_worktree_id(&daemon_session.session);
    daemon_session.agent = None;
    daemon_session.agent_state = None;
    daemon_session.agent_detail = None;
    daemon_session.agent_report_requires_running = true;
    // Dirty-exit backstop clear: `agent: None` clears identity on clients,
    // same as the exit-to-`None` report path.
    Some(AgentStateBroadcast {
        session_id,
        worktree_id,
        agent: None,
        state: None,
        detail: None,
    })
}

// Used by the Unix-only clear_stale_agent_state and by tests on all platforms.
#[cfg(any(unix, test))]
fn agent_command_matches(agent: KnownAgent, command: &str) -> bool {
    let executable = command.split_whitespace().next().unwrap_or(command);
    let executable = Path::new(executable)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(executable);
    match agent {
        KnownAgent::ClaudeCode => executable == "claude",
        KnownAgent::Codex => executable == "codex",
    }
}
// Used by the Unix-only clear_stale_agent_state and by tests on all platforms.
#[cfg(any(unix, test))]
fn foreground_command_is_shell(command: &str) -> bool {
    let executable = command.split_whitespace().next().unwrap_or(command);
    let executable = Path::new(executable)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(executable)
        .trim_start_matches('-');
    matches!(executable, "sh" | "bash" | "zsh" | "fish" | "nu" | "xonsh")
}

fn session_worktree_id(session: &Session) -> Option<WorktreeId> {
    match session.parent {
        SessionParent::Worktree(worktree_id) => Some(worktree_id),
        SessionParent::Project(_) => None,
    }
}

fn path_display(path: &Path) -> &str {
    path.to_str().unwrap_or("<non-utf8>")
}

/// Replay every session's scrollback to one client, then open its output gate.
///
/// MUST run on the dispatcher thread (it is invoked only from the
/// `DispatchMsg::ReplayToClient` arm). Because the dispatcher is single-threaded,
/// no `PtyEvent::Output` is broadcast while this runs, so the snapshot taken here
/// and the live stream that resumes afterwards cannot interleave. Each session's
/// scrollback comes from the authoritative broadcast log — NOT `pty.scrollback()`
/// — so it equals exactly the bytes already broadcast, with no in-flight gap and
/// no duplication once the gate opens.
///
/// The gate is opened LAST. If a send fails partway (client disconnected), we
/// return the error with the gate still closed, so a half-replayed client never
/// starts receiving live output.
fn replay_sessions_to_client(
    state: &Arc<Mutex<DaemonState>>,
    client_id: u64,
) -> Result<(), ProtocolError> {
    let (replay_items, jobs) = {
        let state = state.lock().map_err(|_| internal("state lock poisoned"))?;
        let replay_items = state
            .sessions
            .values()
            .map(|daemon_session| {
                let scrollback = state.broadcaster.replay_snapshot(
                    daemon_session.session.id,
                    &daemon_session.restored_scrollback,
                );
                let command = daemon_session.pty.foreground_command();
                (
                    daemon_session.session.clone(),
                    scrollback,
                    command,
                    daemon_session.agent,
                    daemon_session.agent_state,
                    daemon_session.agent_detail.clone(),
                    daemon_session.output_active,
                )
            })
            .collect::<Vec<_>>();
        let jobs = state
            .jobs
            .iter()
            .map(|(job_id, job)| (*job_id, job.message.clone(), job.kind.map(str::to_string)))
            .collect::<Vec<_>>();
        (replay_items, jobs)
    };

    // Send control-plane messages (SessionOpened, SessionCommand) on the
    // dispatcher thread, then spawn a thread to do the potentially blocking
    // replay output writes. This prevents slow clients from freezing the
    // dispatcher and blocking output delivery for other sessions.
    for (session, _, command, agent, agent_state, agent_detail, output_active) in &replay_items {
        send_event_to_client(
            state,
            client_id,
            Event::SessionOpened {
                session: session.clone(),
                agent: *agent,
                agent_state: *agent_state,
                agent_detail: agent_detail.clone(),
                // Attach with the current gate value so the client does not
                // assume idle/active before the next edge-triggered event.
                output_active: *output_active,
            },
        )?;
        send_event_to_client(
            state,
            client_id,
            Event::SessionCommand {
                session_id: session.id,
                command: command.clone(),
            },
        )?;
    }

    drain_pending_agent_state_events(state, client_id)?;

    for (job_id, message, kind) in &jobs {
        send_event_to_client(
            state,
            client_id,
            Event::JobProgress {
                job_id: *job_id,
                status: JobStatus::Running,
                message: message.clone(),
                kind: kind.clone(),
            },
        )?;
    }

    // Spawn a thread to send replay output and open the gate so slow clients
    // don't block the dispatcher. Output that arrives on the dispatcher while
    // the gate is still closed is buffered in `sink.pending`; we drain that
    // buffer and set `live=true` under the pending lock so the ordering is
    // strictly: snapshot bytes → pending bytes → live broadcasts.
    let state_clone = Arc::clone(state);
    thread::spawn(move || {
        for (session, scrollback, _, _, _, _, _) in &replay_items {
            if !scrollback.is_empty() {
                let _ = send_output_to_client(&state_clone, client_id, session.id, scrollback);
            }
        }

        // Drain buffered output and open the gate. Hold the pending lock while
        // writing the drained bytes so no live broadcast can interleave between
        // the drain and live=true being stored.
        let sink = state_clone
            .lock()
            .ok()
            .and_then(|s| s.clients.get(&client_id).map(Arc::clone));
        if let Some(sink) = sink {
            if let Ok(mut pending) = sink.pending.lock() {
                for (session_id, bytes) in pending.drain(..) {
                    let _ = write_output_to_sink(&sink, session_id, &bytes);
                }
                sink.live.store(true, Ordering::SeqCst);
            }
            if let Ok(mut pending_job_events) = sink.pending_job_events.lock() {
                for event in pending_job_events.drain(..) {
                    let _ = write_control_to_sink(&sink, &ControlMessage::event(event));
                }
                sink.jobs_live.store(true, Ordering::SeqCst);
            }
        }
        // Mirror the gate in the broadcaster so the unit-tested model stays consistent.
        if let Ok(mut state) = state_clone.lock() {
            state.broadcaster.mark_live(client_id);
        }
    });

    Ok(())
}

/// Run a long-running request off the per-client request loop as a **Job** (ADR
/// 0008). Replies to the requester immediately with `JobStarted { job_id }`, then
/// releases the worker and best-effort broadcasts `JobProgress(running)`.
/// Completion still arrives as `JobProgress(<terminal>)` + `JobCompleted { response }`.
/// Keeping the release ahead of the running broadcast prevents a wedged peer
/// client from blocking the job before it starts.
///
/// The worker receives the shared [`JobControl`] so it can register a cancellable
/// child (the Draft Generator's provider tree) and check `is_cancelled()`. A
/// cancelled Job reports `Cancelled` only when `work` also failed — if the work
/// already succeeded before the cancel flag was set, the successful response is
/// preserved. The Job is removed from the registry the moment `work` returns, so
/// a racing `CancelJob` after completion is a no-op. Worker panics are caught and
/// reported as failed completions so the frontend promise never hangs.
///
/// This generalizes the former `spawn_response_task`: instead of replying once by
/// request id, completion travels the broadcast bus keyed by job id, so every
/// attached GUI observes the same lifecycle.
fn start_job<F>(
    name: &'static str,
    state: &Arc<Mutex<DaemonState>>,
    client_id: u64,
    request_id: u64,
    kind: Option<&'static str>,
    progress: Option<&str>,
    work: F,
) -> Result<(), ProtocolError>
where
    F: FnOnce(&Arc<Mutex<DaemonState>>, &JobControl) -> Result<Response, ProtocolError>
        + Send
        + 'static,
{
    let job_id = JobId::new();
    let control = Arc::new(JobControl::default());
    let message = progress.map(str::to_string);
    {
        let mut guard = state.lock().map_err(|_| internal("state lock poisoned"))?;
        guard.jobs.insert(
            job_id,
            ActiveJob {
                control: Arc::clone(&control),
                kind,
                message: message.clone(),
            },
        );
    }

    let worker_state = Arc::clone(state);
    let (start_tx, start_rx) = mpsc::channel();
    let spawn = thread::Builder::new().name(name.into()).spawn(move || {
        if !start_rx.recv().unwrap_or(false) {
            if let Ok(mut guard) = worker_state.lock() {
                guard.jobs.remove(&job_id);
            }
            return;
        }

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            work(&worker_state, &control)
        }));
        let result = match result {
            Ok(inner) => inner,
            Err(_) => Err(ProtocolError::new(
                ErrorCode::Internal,
                "job worker panicked",
            )),
        };
        // Drop the registry entry first so a late CancelJob can't match this id.
        if let Ok(mut guard) = worker_state.lock() {
            guard.jobs.remove(&job_id);
        }
        let (status, response) = match result {
            Ok(response) => (JobStatus::Succeeded, response),
            Err(_) if control.is_cancelled() => (
                JobStatus::Cancelled,
                Response::Error {
                    error: ProtocolError::new(ErrorCode::Unavailable, "job cancelled")
                        .retryable(true),
                },
            ),
            Err(error) => (JobStatus::Failed, Response::Error { error }),
        };
        let _ = broadcast_job_event(
            &worker_state,
            Event::JobProgress {
                job_id,
                status,
                message: None,
                kind: None,
            },
        );
        let _ = broadcast_job_event(
            &worker_state,
            Event::JobCompleted {
                job_id,
                response: Box::new(response),
            },
        );
    });

    if let Err(err) = spawn {
        if let Ok(mut guard) = state.lock() {
            guard.jobs.remove(&job_id);
        }
        return Err(internal(format!("failed to spawn {name}: {err}")));
    }

    // Reply only after the worker thread exists; then release it immediately so a
    // blocked peer-client broadcast cannot prevent the Job from starting.
    if let Err(err) = send_response(
        state,
        client_id,
        request_id,
        Response::JobStarted { job_id },
    ) {
        if let Ok(mut guard) = state.lock() {
            guard.jobs.remove(&job_id);
        }
        let _ = start_tx.send(false);
        return Err(err);
    }
    if start_tx.send(true).is_err() {
        if let Ok(mut guard) = state.lock() {
            guard.jobs.remove(&job_id);
        }
        return Err(internal(format!("failed to release {name} worker")));
    }
    let _ = broadcast_job_event(
        state,
        Event::JobProgress {
            job_id,
            status: JobStatus::Running,
            message,
            kind: kind.map(str::to_string),
        },
    );
    Ok(())
}

/// Dispatch the long-running request `request` as a **Job**. Reached both from a
/// `StartJob` wrapper and from the bare long-op requests (migrated off the inline
/// request loop, ADR 0008). Fast git reads (`status`/`diff`) never come here —
/// they stay synchronous.
fn dispatch_job(
    state: &Arc<Mutex<DaemonState>>,
    client_id: u64,
    request_id: u64,
    request: JobRequest,
) -> Result<(), ProtocolError> {
    match request {
        JobRequest::CloneProject {
            remote_url,
            destination,
            name,
        } => start_job(
            "hitch-clone",
            state,
            client_id,
            request_id,
            Some("clone"),
            Some("Cloning…"),
            move |state, control| {
                let git = {
                    let guard = state.lock().map_err(|_| internal("state lock poisoned"))?;
                    guard.git.clone()
                };
                let root = clone_project(
                    &git,
                    &remote_url,
                    &destination,
                    name.as_deref(),
                    Some(control),
                )?;
                let project = add_project_from_root(state, &root, name.as_deref())?;
                let _ = broadcast_event(
                    state,
                    Event::ProjectUpdated {
                        project: project.clone(),
                    },
                );
                Ok(Response::Projects {
                    projects: vec![project],
                })
            },
        ),
        JobRequest::CreateWorktree {
            project_id,
            branch,
            base,
            mode,
        } => start_job(
            "hitch-create-worktree",
            state,
            client_id,
            request_id,
            Some("create-worktree"),
            Some("Creating worktree…"),
            move |state, control| {
                let worktree =
                    create_worktree(state, project_id, branch, base, mode, Some(control))?;
                let _ = broadcast_event(
                    state,
                    Event::WorktreeUpdated {
                        worktree: worktree.clone(),
                    },
                );
                Ok(Response::Worktrees {
                    worktrees: vec![worktree],
                })
            },
        ),
        JobRequest::Push { worktree_id } => start_job(
            "hitch-push",
            state,
            client_id,
            request_id,
            Some("push"),
            Some("Pushing…"),
            move |state, control| do_push(state, worktree_id, control),
        ),
        JobRequest::Fetch { worktree_id } => start_job(
            "hitch-fetch",
            state,
            client_id,
            request_id,
            Some("fetch"),
            Some("Fetching…"),
            move |state, control| do_fetch(state, worktree_id, control),
        ),
        JobRequest::Pull { worktree_id } => start_job(
            "hitch-pull",
            state,
            client_id,
            request_id,
            Some("pull"),
            Some("Pulling…"),
            move |state, control| do_pull(state, worktree_id, control),
        ),
        JobRequest::PrStatus { worktree_id } => start_job(
            "hitch-pr-status",
            state,
            client_id,
            request_id,
            Some("pr-status"),
            None,
            move |state, control| {
                let pr = pr_status(state, worktree_id, control)?;
                Ok(Response::PrStatus { pr })
            },
        ),
        JobRequest::ProjectPrStatuses { project_id } => start_job(
            "hitch-project-pr-statuses",
            state,
            client_id,
            request_id,
            Some("pr-status"),
            None,
            move |state, control| {
                let statuses = project_pr_statuses(state, project_id, control)?;
                Ok(Response::ProjectPrStatuses { statuses })
            },
        ),
        JobRequest::CreatePullRequest {
            worktree_id,
            title,
            body,
            base,
            draft,
        } => start_job(
            "hitch-create-pr",
            state,
            client_id,
            request_id,
            Some("create-pr"),
            Some("Creating pull request…"),
            move |state, control| {
                do_create_pr(state, worktree_id, title, body, base, draft, control)
            },
        ),
        JobRequest::ListDraftModels { provider, settings } => start_job(
            "hitch-draft-models",
            state,
            client_id,
            request_id,
            Some("draft-models"),
            None,
            move |state, control| {
                let models = list_draft_models(state, provider, settings, Some(control))?;
                Ok(Response::DraftModels { provider, models })
            },
        ),
        JobRequest::GenerateCommitDraft {
            worktree_id,
            settings,
        } => start_job(
            "hitch-commit-draft",
            state,
            client_id,
            request_id,
            Some("commit-draft"),
            Some("Generating commit message…"),
            move |state, control| {
                let draft = generate_commit_draft(state, worktree_id, settings, Some(control))?;
                Ok(Response::CommitDraft { draft })
            },
        ),
        JobRequest::GeneratePullRequestDraft {
            worktree_id,
            base,
            settings,
        } => start_job(
            "hitch-pr-draft",
            state,
            client_id,
            request_id,
            Some("pr-draft"),
            Some("Generating PR description…"),
            move |state, control| {
                let draft =
                    generate_pull_request_draft(state, worktree_id, base, settings, Some(control))?;
                Ok(Response::PullRequestDraft { draft })
            },
        ),
    }
}

fn do_push(
    state: &Arc<Mutex<DaemonState>>,
    worktree_id: WorktreeId,
    control: &JobControl,
) -> Result<Response, ProtocolError> {
    let (git, worktree) = refreshed_worktree_context(state, worktree_id)?;
    git.push_with_control(&worktree.path, "origin", &worktree.branch, true, control)
        .map_err(git_error)?;
    Ok(Response::Ack)
}
fn do_fetch(
    state: &Arc<Mutex<DaemonState>>,
    worktree_id: WorktreeId,
    control: &JobControl,
) -> Result<Response, ProtocolError> {
    let (git, worktree) = refreshed_worktree_context(state, worktree_id)?;
    git.fetch_with_control(&worktree.path, "origin", control)
        .map_err(git_error)?;
    Ok(Response::Ack)
}

fn do_pull(
    state: &Arc<Mutex<DaemonState>>,
    worktree_id: WorktreeId,
    control: &JobControl,
) -> Result<Response, ProtocolError> {
    let (git, worktree) = refreshed_worktree_context(state, worktree_id)?;
    git.pull_with_control(&worktree.path, "origin", &worktree.branch, control)
        .map_err(git_error)?;
    Ok(Response::Ack)
}

fn do_create_pr(
    state: &Arc<Mutex<DaemonState>>,
    worktree_id: WorktreeId,
    title: String,
    body: Option<String>,
    base: Option<String>,
    draft: bool,
    control: &JobControl,
) -> Result<Response, ProtocolError> {
    let (git, worktree) = refreshed_worktree_context(state, worktree_id)?;
    let url = git
        .create_pr_with_control(
            &worktree.path,
            &CreatePrRequest {
                title,
                body,
                base,
                head: Some(worktree.branch),
                remote: None,
                draft,
            },
            control,
        )
        .map_err(git_error)?;
    Ok(Response::PullRequestCreated { url })
}

fn send_response(
    state: &Arc<Mutex<DaemonState>>,
    client_id: u64,
    request_id: u64,
    response: Response,
) -> Result<(), ProtocolError> {
    send_control_to_client(
        state,
        client_id,
        &ControlMessage::response(request_id, response),
    )
}

fn send_event_to_client(
    state: &Arc<Mutex<DaemonState>>,
    client_id: u64,
    event: Event,
) -> Result<(), ProtocolError> {
    send_control_to_client(state, client_id, &ControlMessage::event(event))
}

fn send_output_to_client(
    state: &Arc<Mutex<DaemonState>>,
    client_id: u64,
    session_id: SessionId,
    bytes: &[u8],
) -> Result<(), ProtocolError> {
    let sink = client_sink(state, client_id)?;
    write_output_to_sink(&sink, session_id, bytes).map_err(|err| {
        ProtocolError::new(
            ErrorCode::Unavailable,
            format!("failed to write to client {client_id}: {err}"),
        )
        .retryable(true)
    })
}

fn send_control_to_client(
    state: &Arc<Mutex<DaemonState>>,
    client_id: u64,
    message: &ControlMessage,
) -> Result<(), ProtocolError> {
    let sink = client_sink(state, client_id)?;
    write_control_to_sink(&sink, message).map_err(|err| {
        ProtocolError::new(
            ErrorCode::Unavailable,
            format!("failed to write to client {client_id}: {err}"),
        )
        .retryable(true)
    })
}

fn client_sink(
    state: &Arc<Mutex<DaemonState>>,
    client_id: u64,
) -> Result<Arc<ClientSink>, ProtocolError> {
    let state = state.lock().map_err(|_| internal("state lock poisoned"))?;
    state
        .clients
        .get(&client_id)
        .cloned()
        .ok_or_else(|| ProtocolError::new(ErrorCode::Unavailable, "client disconnected"))
}

fn broadcast_event(state: &Arc<Mutex<DaemonState>>, event: Event) -> Result<(), ProtocolError> {
    let message = ControlMessage::event(event);
    broadcast_with(state, |sink| write_control_to_sink(sink, &message));
    Ok(())
}

fn broadcast_agent_state_event(
    state: &Arc<Mutex<DaemonState>>,
    event: Event,
) -> Result<(), ProtocolError> {
    let clients = {
        let state = state.lock().map_err(|_| internal("state lock poisoned"))?;
        state
            .clients
            .iter()
            .map(|(id, sink)| (*id, Arc::clone(sink)))
            .collect::<Vec<_>>()
    };

    let mut dead = Vec::new();
    for (id, sink) in clients {
        let write_result = if sink.agent_state_live.load(Ordering::SeqCst) {
            write_control_to_sink(&sink, &ControlMessage::event(event.clone()))
        } else {
            let mut pending = sink
                .pending_agent_state_events
                .lock()
                .map_err(|_| internal("agent-state replay buffer lock poisoned"))?;
            if sink.agent_state_live.load(Ordering::SeqCst) {
                drop(pending);
                write_control_to_sink(&sink, &ControlMessage::event(event.clone()))
            } else {
                pending.push(event.clone());
                Ok(())
            }
        };
        if write_result.is_err() {
            dead.push(id);
        }
    }

    if !dead.is_empty() {
        if let Ok(mut state) = state.lock() {
            for id in dead {
                state.clients.remove(&id);
                state.broadcaster.forget_client(id);
            }
        }
    }

    Ok(())
}

fn drain_pending_agent_state_events(
    state: &Arc<Mutex<DaemonState>>,
    client_id: u64,
) -> Result<(), ProtocolError> {
    let sink = client_sink(state, client_id)?;
    let mut pending = sink
        .pending_agent_state_events
        .lock()
        .map_err(|_| internal("agent-state replay buffer lock poisoned"))?;
    for event in pending.drain(..) {
        write_control_to_sink(&sink, &ControlMessage::event(event)).map_err(|err| {
            ProtocolError::new(
                ErrorCode::Unavailable,
                format!("failed to write buffered agent-state event to client {client_id}: {err}"),
            )
            .retryable(true)
        })?;
    }
    sink.agent_state_live.store(true, Ordering::SeqCst);
    Ok(())
}

fn broadcast_job_event(state: &Arc<Mutex<DaemonState>>, event: Event) -> Result<(), ProtocolError> {
    let clients = {
        let state = state.lock().map_err(|_| internal("state lock poisoned"))?;
        state
            .clients
            .iter()
            .map(|(id, sink)| (*id, Arc::clone(sink)))
            .collect::<Vec<_>>()
    };

    let mut dead = Vec::new();
    for (id, sink) in clients {
        let write_result = if sink.jobs_live.load(Ordering::SeqCst) {
            write_control_to_sink(&sink, &ControlMessage::event(event.clone()))
        } else {
            let mut pending = sink
                .pending_job_events
                .lock()
                .map_err(|_| internal("job replay buffer lock poisoned"))?;
            if sink.jobs_live.load(Ordering::SeqCst) {
                drop(pending);
                write_control_to_sink(&sink, &ControlMessage::event(event.clone()))
            } else {
                pending.push(event.clone());
                Ok(())
            }
        };
        if write_result.is_err() {
            dead.push(id);
        }
    }

    if !dead.is_empty() {
        if let Ok(mut state) = state.lock() {
            for id in dead {
                state.clients.remove(&id);
            }
        }
    }
    Ok(())
}

fn broadcast_session_output(
    state: &Arc<Mutex<DaemonState>>,
    session_id: SessionId,
    bytes: &[u8],
) -> Result<(), ProtocolError> {
    // Output gate: a client registered at accept time but whose replay thread
    // has not yet opened the gate must NOT receive live output before its
    // snapshot. While the gate is closed we buffer into `sink.pending` instead
    // of skipping, so bytes that arrive during the replay write window are not
    // lost. The replay thread drains `pending` and sets `live=true` under the
    // pending lock, so ordering is: snapshot → pending → live. Generic
    // control-plane broadcasts still ignore this gate; job events use
    // `broadcast_job_event`'s separate replay buffer.
    let clients = match state.lock() {
        Ok(state) => state
            .clients
            .iter()
            .map(|(id, sink)| (*id, Arc::clone(sink)))
            .collect::<Vec<_>>(),
        Err(_) => return Ok(()),
    };

    let mut dead = Vec::new();
    for (id, sink) in clients {
        if !sink.live.load(Ordering::SeqCst) {
            // Gate closed: buffer rather than drop. Acquire the pending lock and
            // double-check live in case the replay thread opened the gate between
            // the first load and acquiring the lock.
            if let Ok(mut pending) = sink.pending.lock() {
                if !sink.live.load(Ordering::SeqCst) {
                    pending.push((session_id, bytes.to_vec()));
                    continue;
                }
                // Gate opened while we waited for the lock — fall through.
            }
        }
        if write_output_to_sink(&sink, session_id, bytes).is_err() {
            dead.push(id);
        }
    }

    if !dead.is_empty() {
        if let Ok(mut state) = state.lock() {
            for id in dead {
                state.clients.remove(&id);
            }
        }
    }
    Ok(())
}

/// Broadcast to every registered client. Used by the control plane, which has no
/// readiness gate: events flow to all clients the moment they connect.
fn broadcast_with<F>(state: &Arc<Mutex<DaemonState>>, send: F)
where
    F: FnMut(&ClientSink) -> io::Result<()>,
{
    broadcast_with_filter(state, |_| true, send);
}

/// Broadcast to the clients accepted by `keep`, dropping any whose write fails.
/// The output path passes a `keep` that admits only live (replayed) clients; the
/// control plane passes `|_| true`. Filtering here, rather than inside `send`,
/// keeps a skipped client off the `dead` list — a closed gate is not a failure.
fn broadcast_with_filter<K, F>(state: &Arc<Mutex<DaemonState>>, keep: K, mut send: F)
where
    K: Fn(&ClientSink) -> bool,
    F: FnMut(&ClientSink) -> io::Result<()>,
{
    let clients = match state.lock() {
        Ok(state) => state
            .clients
            .iter()
            .map(|(id, sink)| (*id, Arc::clone(sink)))
            .collect::<Vec<_>>(),
        Err(_) => return,
    };

    let mut dead = Vec::new();
    for (id, sink) in clients {
        if !keep(&sink) {
            continue;
        }
        if send(&sink).is_err() {
            dead.push(id);
        }
    }

    if !dead.is_empty() {
        if let Ok(mut state) = state.lock() {
            for id in dead {
                state.clients.remove(&id);
            }
        }
    }
}

fn write_control_to_sink(sink: &ClientSink, message: &ControlMessage) -> io::Result<()> {
    let bytes = encode_control_message(message).map_err(io::Error::other)?;
    let mut writer = sink.writer.lock().map_err(|_| poisoned("client writer"))?;
    writer.write_all(&bytes)?;
    writer.flush()
}

fn write_output_to_sink(sink: &ClientSink, session_id: SessionId, bytes: &[u8]) -> io::Result<()> {
    let event = ControlMessage::event(Event::SessionOutput {
        session_id,
        byte_count: bytes.len() as u32,
    });
    let control = encode_control_message(&event).map_err(io::Error::other)?;
    let payload = encode_pty_frame(bytes).map_err(io::Error::other)?;

    let mut writer = sink.writer.lock().map_err(|_| poisoned("client writer"))?;
    writer.write_all(&control)?;
    writer.write_all(&payload)?;
    writer.flush()
}

fn cancel_active_jobs(state: &Arc<Mutex<DaemonState>>) {
    let jobs = match state.lock() {
        Ok(state) => state
            .jobs
            .values()
            .map(|job| Arc::clone(&job.control))
            .collect::<Vec<_>>(),
        Err(_) => Vec::new(),
    };
    for control in jobs {
        control.cancel();
    }
}

fn wait_for_jobs_to_finish(state: &Arc<Mutex<DaemonState>>) {
    loop {
        let done = match state.lock() {
            Ok(state) => state.jobs.is_empty(),
            Err(_) => true,
        };
        if done {
            return;
        }
        thread::sleep(Duration::from_millis(25));
    }
}
fn kill_all_sessions(state: &Arc<Mutex<DaemonState>>) {
    let sessions = match state.lock() {
        Ok(mut state) => state
            .sessions
            .drain()
            .map(|(_, session)| session.pty)
            .collect::<Vec<_>>(),
        Err(_) => Vec::new(),
    };
    for session in sessions {
        let _ = session.kill();
    }
}

/// Per-instance data directory. The cross-platform layout is owned by
/// [`hitch_proto::transport::default_data_dir`] so the daemon and the GUI's
/// `daemon_log_path` never drift onto different roots.
fn data_dir() -> PathBuf {
    hitch_proto::transport::default_data_dir()
}

fn default_store_path() -> PathBuf {
    data_dir().join("hitch.sqlite")
}

fn default_managed_worktree_root() -> PathBuf {
    data_dir().join("worktrees")
}

fn default_hook_helper_path() -> PathBuf {
    std::env::current_exe()
        .ok()
        .map(|path| hook_helper_path_for_daemon_exe(&path))
        .unwrap_or_else(|| PathBuf::from("hitch-hook"))
}

fn hook_helper_path_for_daemon_exe(exe: &Path) -> PathBuf {
    let Some(parent) = exe.parent() else {
        return PathBuf::from("hitch-hook");
    };
    let Some(file_name) = exe.file_name().and_then(|name| name.to_str()) else {
        return parent.join("hitch-hook");
    };
    let suffix = file_name.strip_prefix("hitch-daemon").unwrap_or_default();
    parent.join(format!("hitch-hook{suffix}"))
}

fn store_error(err: hitch_store::StoreError) -> ProtocolError {
    let code = match err {
        hitch_store::StoreError::NotFound(_) => ErrorCode::NotFound,
        hitch_store::StoreError::InvalidSessionParent(_) => ErrorCode::InvalidRequest,
        _ => ErrorCode::StoreFailed,
    };
    ProtocolError::new(code, err.to_string())
}

fn git_error(err: hitch_git::GitError) -> ProtocolError {
    ProtocolError::new(ErrorCode::GitFailed, err.to_string())
}

fn internal(message: impl Into<String>) -> ProtocolError {
    ProtocolError::new(ErrorCode::Internal, message)
}

fn poisoned(name: &'static str) -> io::Error {
    io::Error::other(format!("{name} lock poisoned"))
}

#[cfg(test)]
mod tests {
    use super::{best_pr_for_branch, OutputBroadcaster};
    use hitch_core::SessionId;

    fn git_pr(number: u64, state: &str) -> hitch_git::PrInfo {
        hitch_git::PrInfo {
            number,
            url: format!("https://example.test/pr/{number}"),
            state: state.to_string(),
            draft: false,
        }
    }

    #[test]
    fn best_pr_for_branch_matches_by_branch_and_prefers_open_then_newest() {
        let prs = vec![
            ("feature".to_string(), git_pr(1, "MERGED")),
            ("feature".to_string(), git_pr(7, "OPEN")),
            ("feature".to_string(), git_pr(9, "CLOSED")),
            ("other".to_string(), git_pr(20, "OPEN")),
        ];
        // Open wins over a higher-numbered closed/merged PR on the same branch.
        assert_eq!(best_pr_for_branch(&prs, "feature").unwrap().number, 7);
        // A branch with no PR yields None rather than borrowing another's.
        assert!(best_pr_for_branch(&prs, "missing").is_none());
    }

    #[test]
    fn best_pr_for_branch_falls_back_to_newest_when_none_open() {
        let prs = vec![
            ("topic".to_string(), git_pr(3, "MERGED")),
            ("topic".to_string(), git_pr(11, "CLOSED")),
        ];
        // No open PR: the most recent (highest number) represents the branch.
        assert_eq!(best_pr_for_branch(&prs, "topic").unwrap().number, 11);
    }

    // A test harness that mirrors the dispatcher thread's single-threaded
    // serialization: the only legal operations are `record_output` (the
    // `PtyEvent::Output` handler) and `replay` (the `ReplayToClient` handler).
    // Both run "on the dispatcher thread", so interleaving them here reproduces
    // exactly the ordering the real daemon enforces — without any sockets or
    // threads.
    struct Harness {
        broadcaster: OutputBroadcaster,
        // What each client has actually received, in order: the replay snapshot
        // first, then every live broadcast after it went live.
        received: std::collections::HashMap<u64, Vec<u8>>,
        // The session's persisted scrollback at restore time. Empty for sessions
        // born in this daemon run; non-empty for sessions restored from the store.
        restored: Vec<u8>,
        session_id: SessionId,
    }

    impl Harness {
        fn new(restored: &[u8]) -> Self {
            Self {
                broadcaster: OutputBroadcaster::default(),
                received: std::collections::HashMap::new(),
                restored: restored.to_vec(),
                session_id: SessionId::new(),
            }
        }

        fn connect(&mut self, client_id: u64) {
            self.received.entry(client_id).or_default();
        }

        // The `PtyEvent::Output` path: append to the authoritative log, then
        // broadcast to every client whose readiness gate is open.
        fn record_output(&mut self, bytes: &[u8]) {
            self.broadcaster.record_output(self.session_id, bytes);
            let live: Vec<u64> = self
                .received
                .keys()
                .copied()
                .filter(|id| self.broadcaster.is_live(*id))
                .collect();
            for id in live {
                self.received.get_mut(&id).unwrap().extend_from_slice(bytes);
            }
        }

        // The `ReplayToClient` path: deliver the authoritative snapshot to the
        // client, then open its gate so subsequent output reaches it live.
        fn replay(&mut self, client_id: u64) {
            let snapshot = self
                .broadcaster
                .replay_snapshot(self.session_id, &self.restored);
            self.received
                .get_mut(&client_id)
                .unwrap()
                .extend_from_slice(&snapshot);
            self.broadcaster.mark_live(client_id);
        }

        fn received(&self, client_id: u64) -> &[u8] {
            &self.received[&client_id]
        }
    }

    #[test]
    fn hook_helper_path_tracks_tauri_sidecar_suffix() {
        assert_eq!(
            super::hook_helper_path_for_daemon_exe(std::path::Path::new(
                "/app/binaries/hitch-daemon-aarch64-apple-darwin"
            )),
            std::path::PathBuf::from("/app/binaries/hitch-hook-aarch64-apple-darwin")
        );
        assert_eq!(
            super::hook_helper_path_for_daemon_exe(std::path::Path::new(
                "/target/debug/hitch-daemon"
            )),
            std::path::PathBuf::from("/target/debug/hitch-hook")
        );
    }

    #[test]
    fn output_before_replay_is_in_snapshot_and_not_redelivered_live() {
        // The race: a live Output is appended to the log before the client's
        // replay runs. It must show up in the replay snapshot exactly once and
        // never be broadcast again afterwards (no duplication).
        let mut h = Harness::new(b"");
        h.connect(1);
        h.record_output(b"pre-replay");
        h.replay(1);

        assert_eq!(
            h.received(1),
            b"pre-replay",
            "output before replay must arrive exactly once, inside the snapshot"
        );
    }

    #[cfg(unix)]
    #[test]
    fn agent_state_before_replay_waits_until_session_snapshot_is_sent() {
        use hitch_core::{AgentState, WorktreeId};
        use hitch_proto::{ControlLineDecoder, ControlMessage, Event, KnownAgent};
        use std::io::Read as _;
        use std::os::unix::net::UnixStream;
        use std::path::PathBuf;
        use std::sync::{
            atomic::{AtomicBool, Ordering},
            Arc, Mutex,
        };
        use std::time::{Duration, SystemTime, UNIX_EPOCH};

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("hitch-daemon-agent-replay-{nonce}"));
        std::fs::create_dir_all(&dir).unwrap();
        let store_path = dir.join("state.db");
        let config = super::DaemonConfig {
            socket_path: dir.join("daemon.sock"),
            store_path: store_path.clone(),
            managed_root: dir.join("managed"),
            hook_helper: dir.join("hook"),
            git: PathBuf::from("git"),
            gh: PathBuf::from("gh"),
            draft_provider: crate::drafts::DraftProviderConfig::from_env().unwrap(),
        };
        let store = hitch_store::Store::open(&store_path).unwrap();
        let state = Arc::new(std::sync::Mutex::new(super::DaemonState::new(
            store, config,
        )));

        let (mut reader, writer) = UnixStream::pair().unwrap();
        let sink = Arc::new(super::ClientSink {
            writer: Mutex::new(super::DaemonStream::new(writer)),
            live: AtomicBool::new(true),
            jobs_live: AtomicBool::new(true),
            agent_state_live: AtomicBool::new(false),
            pending: Mutex::new(Vec::new()),
            pending_job_events: Mutex::new(Vec::new()),
            pending_agent_state_events: Mutex::new(Vec::new()),
        });
        {
            let mut guard = state.lock().unwrap();
            guard.clients.insert(1, Arc::clone(&sink));
        }

        let event = Event::AgentState {
            session_id: Some(SessionId::new()),
            worktree_id: Some(WorktreeId::new()),
            agent: Some(KnownAgent::ClaudeCode),
            state: Some(AgentState::Running),
            detail: None,
        };

        super::broadcast_agent_state_event(&state, event.clone()).unwrap();
        assert!(!sink.agent_state_live.load(Ordering::SeqCst));
        assert_eq!(sink.pending_agent_state_events.lock().unwrap().len(), 1);

        super::drain_pending_agent_state_events(&state, 1).unwrap();
        assert!(sink.agent_state_live.load(Ordering::SeqCst));
        assert!(sink.pending_agent_state_events.lock().unwrap().is_empty());

        reader
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut decoder = ControlLineDecoder::new();
        let mut buf = [0u8; 8192];
        let delivered = loop {
            let n = reader.read(&mut buf).expect("read buffered agent-state");
            assert!(n > 0, "client sink closed before delivering the event");
            if let Some(delivered) =
                decoder
                    .push(&buf[..n])
                    .unwrap()
                    .into_iter()
                    .find_map(|message| match message {
                        ControlMessage::Event { event } => Some(event),
                        _ => None,
                    })
            {
                break delivered;
            }
        };
        assert_eq!(delivered, event);

        drop(state);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn installing_hooks_for_worktree_writes_agent_configs() {
        use hitch_core::{Project, ProjectKind, Worktree};
        use std::path::PathBuf;
        use std::process::Command;
        use std::sync::{Arc, Mutex};
        use std::time::{SystemTime, UNIX_EPOCH};

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("hitch-daemon-hooks-{nonce}"));
        let project_root = dir.join("repo");
        std::fs::create_dir_all(&project_root).unwrap();
        run_git(&project_root, ["init", "--initial-branch=main"]);
        let store_path = dir.join("state.db");
        let config = super::DaemonConfig {
            socket_path: dir.join("daemon.sock"),
            store_path: store_path.clone(),
            managed_root: dir.join("managed"),
            hook_helper: dir.join("hitch-hook"),
            git: PathBuf::from("git"),
            gh: PathBuf::from("gh"),
            draft_provider: crate::drafts::DraftProviderConfig::from_env().unwrap(),
        };
        let store = hitch_store::Store::open(&store_path).unwrap();
        let state = Arc::new(Mutex::new(super::DaemonState::new(store, config)));
        let project = Project::new("hitch", &project_root, ProjectKind::GitBacked);
        let worktree = Worktree::new(project.id, &project_root, "main", true, false);
        {
            let mut guard = state.lock().unwrap();
            guard.store.insert_project(&project).unwrap();
            guard.store.insert_worktree(&worktree).unwrap();
            guard.projects.insert(project.id, project);
            guard.worktrees.insert(worktree.id, worktree.clone());
        }

        super::install_agent_hooks_for_worktree_id(&state, worktree.id).unwrap();

        let claude =
            std::fs::read_to_string(project_root.join(".claude/settings.local.json")).unwrap();
        assert!(claude.contains("hitch-hook"));
        assert!(claude.contains("--agent claude-code"));
        let codex = std::fs::read_to_string(project_root.join(".codex/hooks.json")).unwrap();
        assert!(codex.contains("--agent codex"));
        assert!(codex.contains("PermissionRequest"));
        assert!(!project_root.join(".gitignore").exists());
        let exclude = std::fs::read_to_string(project_root.join(".git/info/exclude")).unwrap();
        assert!(exclude.contains(".claude/settings.local.json"));
        assert!(exclude.contains(".codex/hooks.json"));
        assert_git_status_clean(&project_root);

        drop(state);
        let _ = std::fs::remove_dir_all(&dir);

        fn run_git<const N: usize>(cwd: &std::path::Path, args: [&str; N]) {
            let output = Command::new("git")
                .current_dir(cwd)
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "git failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }

        fn assert_git_status_clean(cwd: &std::path::Path) {
            let output = Command::new("git")
                .current_dir(cwd)
                .args(["status", "--porcelain"])
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "git status failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(
                output.stdout.is_empty(),
                "git status was dirty: {}",
                String::from_utf8_lossy(&output.stdout)
            );
        }
    }

    #[test]
    fn add_project_succeeds_when_hook_install_fails() {
        use std::path::PathBuf;
        use std::process::Command;
        use std::sync::{Arc, Mutex};
        use std::time::{SystemTime, UNIX_EPOCH};

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("hitch-daemon-addproj-{nonce}"));
        let project_root = dir.join("repo");
        std::fs::create_dir_all(&project_root).unwrap();
        run_git(&project_root, ["init", "--initial-branch=main"]);
        // A malformed agent config makes hook installation fail. Adding the
        // project must still succeed (best-effort hooks), so the repo stays
        // usable as a plain terminal.
        std::fs::create_dir_all(project_root.join(".claude")).unwrap();
        std::fs::write(project_root.join(".claude/settings.local.json"), "not json").unwrap();

        let store_path = dir.join("state.db");
        let config = super::DaemonConfig {
            socket_path: dir.join("daemon.sock"),
            store_path: store_path.clone(),
            managed_root: dir.join("managed"),
            hook_helper: dir.join("hitch-hook"),
            git: PathBuf::from("git"),
            gh: PathBuf::from("gh"),
            draft_provider: crate::drafts::DraftProviderConfig::from_env().unwrap(),
        };
        let store = hitch_store::Store::open(&store_path).unwrap();
        let state = Arc::new(Mutex::new(super::DaemonState::new(store, config)));

        let project = super::add_project_from_root(&state, &project_root, None).unwrap();

        let guard = state.lock().unwrap();
        assert!(guard.projects.contains_key(&project.id));
        assert!(
            guard
                .worktrees
                .values()
                .any(|worktree| worktree.project_id == project.id),
            "main worktree should be registered even though hooks failed"
        );
        drop(guard);

        drop(state);
        let _ = std::fs::remove_dir_all(&dir);

        fn run_git<const N: usize>(cwd: &std::path::Path, args: [&str; N]) {
            let output = Command::new("git")
                .current_dir(cwd)
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "git failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    #[test]
    fn reconcile_registers_discovered_worktree_when_hook_install_fails() {
        use std::path::PathBuf;
        use std::process::Command;
        use std::sync::{Arc, Mutex};
        use std::time::{SystemTime, UNIX_EPOCH};

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("hitch-daemon-reconcile-{nonce}"));
        let project_root = dir.join("repo");
        std::fs::create_dir_all(&project_root).unwrap();
        run_git(&project_root, ["init", "--initial-branch=main"]);
        run_git(&project_root, ["config", "user.name", "Hitch Test"]);
        run_git(
            &project_root,
            ["config", "user.email", "hitch@example.test"],
        );
        std::fs::write(project_root.join("tracked.txt"), "initial\n").unwrap();
        run_git(&project_root, ["add", "tracked.txt"]);
        run_git(&project_root, ["commit", "-m", "initial"]);
        // An externally-created linked worktree git knows about but Hitch has
        // not registered yet.
        let feature_path = dir.join("feature");
        run_git(
            &project_root,
            [
                "worktree",
                "add",
                feature_path.to_str().unwrap(),
                "-b",
                "feature",
            ],
        );
        // A malformed agent config makes hook installation fail for that worktree.
        std::fs::create_dir_all(feature_path.join(".claude")).unwrap();
        std::fs::write(feature_path.join(".claude/settings.local.json"), "not json").unwrap();

        let store_path = dir.join("state.db");
        let config = super::DaemonConfig {
            socket_path: dir.join("daemon.sock"),
            store_path: store_path.clone(),
            managed_root: dir.join("managed"),
            hook_helper: dir.join("hitch-hook"),
            git: PathBuf::from("git"),
            gh: PathBuf::from("gh"),
            draft_provider: crate::drafts::DraftProviderConfig::from_env().unwrap(),
        };
        let store = hitch_store::Store::open(&store_path).unwrap();
        let state = Arc::new(Mutex::new(super::DaemonState::new(store, config)));
        // Register the project + main worktree, leaving the linked worktree for
        // reconciliation to discover.
        let project = super::add_project_from_root(&state, &project_root, None).unwrap();

        super::reconcile_discovered_worktrees(&state, project.id);

        let guard = state.lock().unwrap();
        let feature_canonical = feature_path.canonicalize().unwrap();
        assert!(
            guard.worktrees.values().any(|worktree| {
                worktree
                    .path
                    .canonicalize()
                    .map(|path| path == feature_canonical)
                    .unwrap_or(false)
            }),
            "discovered worktree should be registered even though hooks failed"
        );
        drop(guard);

        drop(state);
        let _ = std::fs::remove_dir_all(&dir);

        fn run_git<const N: usize>(cwd: &std::path::Path, args: [&str; N]) {
            let output = Command::new("git")
                .current_dir(cwd)
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "git failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    #[test]
    fn hook_report_without_session_id_is_dropped_even_with_worktree_cwd() {
        use hitch_proto::KnownAgent;
        use std::path::PathBuf;
        use std::sync::{Arc, Mutex};
        use std::time::{SystemTime, UNIX_EPOCH};

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("hitch-daemon-drop-hook-{nonce}"));
        let subdir = dir.join("repo/src");
        std::fs::create_dir_all(&subdir).unwrap();
        let store_path = dir.join("state.db");
        let config = super::DaemonConfig {
            socket_path: dir.join("daemon.sock"),
            store_path: store_path.clone(),
            managed_root: dir.join("managed"),
            hook_helper: dir.join("hitch-hook"),
            git: PathBuf::from("git"),
            gh: PathBuf::from("gh"),
            draft_provider: crate::drafts::DraftProviderConfig::from_env().unwrap(),
        };
        let store = hitch_store::Store::open(&store_path).unwrap();
        let state = Arc::new(Mutex::new(super::DaemonState::new(store, config)));

        let event = super::store_agent_report(
            &state,
            KnownAgent::ClaudeCode,
            Some(hitch_core::AgentState::Running),
            None,
            Some(&subdir),
            Some("ignored".into()),
        )
        .unwrap();
        assert!(event.is_none());

        drop(state);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn agent_command_match_uses_known_executable_name_only() {
        use hitch_proto::KnownAgent;

        assert!(super::agent_command_matches(
            KnownAgent::ClaudeCode,
            "/usr/local/bin/claude"
        ));
        assert!(super::agent_command_matches(
            KnownAgent::Codex,
            "codex --sandbox read-only"
        ));
        assert!(!super::agent_command_matches(KnownAgent::ClaudeCode, "zsh"));
        assert!(super::foreground_command_is_shell("zsh"));
        assert!(super::foreground_command_is_shell("-zsh"));
        assert!(super::foreground_command_is_shell("/bin/bash -l"));
        assert!(!super::foreground_command_is_shell("git status"));
        assert!(!super::foreground_command_is_shell("python -m pytest"));
    }

    #[test]
    fn output_after_replay_is_delivered_live() {
        // Output appended after the client went live must reach it as a live
        // broadcast (no loss).
        let mut h = Harness::new(b"");
        h.connect(1);
        h.replay(1);
        h.record_output(b"post-replay");
        assert_eq!(h.received(1), b"post-replay");
    }

    #[test]
    fn no_output_reaches_a_client_before_its_replay() {
        // Until a client is replayed (and thus made live), the output path must
        // skip it entirely — even though it is a registered client.
        let mut h = Harness::new(b"");
        h.connect(1);
        h.record_output(b"never-seen-without-replay");
        assert!(
            h.received(1).is_empty(),
            "a client must receive nothing before its replay"
        );
    }

    #[test]
    fn brand_new_session_replays_cleanly_from_empty_log() {
        // A session that has never produced output (empty log, no restored
        // scrollback) replays as an empty snapshot, then streams live.
        let mut h = Harness::new(b"");
        h.connect(1);
        h.replay(1);
        assert!(h.received(1).is_empty());
        h.record_output(b"first-bytes");
        assert_eq!(h.received(1), b"first-bytes");
    }

    #[test]
    fn replay_composes_restored_scrollback_then_live_log() {
        // Replay must reproduce the same bytes the old `restored_scrollback +
        // pty.scrollback()` produced: restored bytes first, then everything seen
        // live so far.
        let mut h = Harness::new(b"restored-");
        h.record_output(b"live-1");
        h.connect(1);
        h.replay(1);
        assert_eq!(h.received(1), b"restored-live-1");
        h.record_output(b"live-2");
        assert_eq!(h.received(1), b"restored-live-1live-2");
    }

    #[test]
    fn interleaved_clients_each_get_every_byte_exactly_once() {
        // The core invariant across two clients whose replays straddle live
        // output: every byte reaches each client either in its snapshot or as a
        // post-replay live broadcast, never both, never neither.
        let mut h = Harness::new(b"R");
        h.connect(1);
        h.replay(1); // client 1 goes live first, snapshot = "R"
        h.record_output(b"A"); // live to client 1
        h.connect(2);
        h.record_output(b"B"); // live to client 1; queued in client 2's snapshot
        h.replay(2); // client 2 snapshot = "RAB"
        h.record_output(b"C"); // live to both

        assert_eq!(h.received(1), b"RABC");
        assert_eq!(h.received(2), b"RABC");
    }

    #[test]
    fn live_log_is_bounded_to_scrollback_capacity() {
        // The authoritative log mirrors the reader ring, so it trims its head to
        // the same capacity: a late replay returns the most recent window of live
        // bytes, exactly what `pty.scrollback()` would have. Restored scrollback
        // is a separate buffer prepended at replay (matching the old behaviour),
        // so the snapshot is `restored + capped(live_log)`.
        let mut h = Harness::new(b"R");
        h.connect(1);
        let overflow = 10;
        let blob = vec![b'x'; hitch_pty::DEFAULT_SCROLLBACK_CAPACITY + overflow];
        h.record_output(&blob);
        h.replay(1);
        assert_eq!(
            h.received(1).len(),
            // restored ("R") + the live log capped at capacity.
            1 + hitch_pty::DEFAULT_SCROLLBACK_CAPACITY,
            "live log must be capped at scrollback capacity, restored prepended on top"
        );
    }

    #[test]
    fn rotate_and_open_log_preserves_previous_run_and_opens_fresh() {
        use std::io::Write;
        use std::time::{SystemTime, UNIX_EPOCH};

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("hitch-log-rotate-{nonce}"));
        std::fs::create_dir_all(&dir).unwrap();
        let log_path = dir.join("daemon.log");
        let prev_path = dir.join("daemon.log.prev");

        // First run: no prior log, nothing to rotate, fresh file opened empty.
        {
            let mut file = super::rotate_and_open_log(&log_path).unwrap();
            writeln!(file, "first run crash").unwrap();
        }
        assert!(!prev_path.exists(), "first run must not create a .prev");

        // Second run: the prior log rotates to .prev and a fresh log opens. The
        // crash trace from the first run survives its respawn in .prev.
        {
            let mut file = super::rotate_and_open_log(&log_path).unwrap();
            writeln!(file, "second run starting").unwrap();
        }
        let prev = std::fs::read_to_string(&prev_path).unwrap();
        assert!(prev.contains("first run crash"));
        let current = std::fs::read_to_string(&log_path).unwrap();
        assert!(current.contains("second run starting"));
        assert!(
            !current.contains("first run crash"),
            "fresh log must be truncated, not appended to"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn job_control_cancel_kills_registered_child_process_group() {
        use hitch_process::ProcessTree;
        use std::process::{Command, Stdio};
        use std::time::{Duration, Instant};

        // A long-lived child in a registered process tree, exactly as the Draft
        // Generator spawns its provider. On Unix the tree is a process group;
        // `CancelJob` must reach it so it dies promptly rather than running its
        // full sleep.
        let control = super::JobControl::default();
        let (mut child, process_tree) = {
            let mut cmd = Command::new("sleep");
            cmd.arg("30")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            ProcessTree::spawn(&mut cmd).expect("spawn sleep")
        };
        control.set_process_tree(Some(process_tree));

        control.cancel();
        assert!(control.is_cancelled());

        let started = Instant::now();
        loop {
            match child.try_wait().expect("try_wait") {
                Some(_) => break,
                None if started.elapsed() > Duration::from_secs(5) => {
                    let _ = child.kill();
                    panic!("cancel did not kill the registered child process group");
                }
                None => std::thread::sleep(Duration::from_millis(20)),
            }
        }
    }

    #[cfg(windows)]
    #[test]
    fn job_control_cancel_kills_registered_windows_process_tree() {
        use hitch_process::ProcessTree;
        use std::process::{Command, Stdio};
        use std::time::{Duration, Instant};

        // A PowerShell provider that starts a long-lived child and then stays
        // alive itself. On Windows the registered `ProcessTree` is a Job Object;
        // `CancelJob` must terminate it promptly instead of waiting for either
        // sleep to finish.
        let control = super::JobControl::default();
        let (mut child, process_tree) = {
            let mut cmd = Command::new("powershell.exe");
            cmd.arg("-NoProfile")
                .arg("-NonInteractive")
                .arg("-ExecutionPolicy")
                .arg("Bypass")
                .arg("-Command")
                .arg(
                    "$child = Start-Process -FilePath powershell.exe -ArgumentList @('-NoProfile','-NonInteractive','-Command','Start-Sleep -Seconds 30') -PassThru; Start-Sleep -Seconds 30",
                )
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            ProcessTree::spawn(&mut cmd).expect("spawn powershell")
        };
        control.set_process_tree(Some(process_tree));

        control.cancel();
        assert!(control.is_cancelled());

        let started = Instant::now();
        loop {
            match child.try_wait().expect("try_wait") {
                Some(_) => break,
                None if started.elapsed() > Duration::from_secs(5) => {
                    let _ = child.kill();
                    panic!("cancel did not kill the registered Windows process tree");
                }
                None => std::thread::sleep(Duration::from_millis(20)),
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn start_job_releases_worker_before_running_broadcast() {
        use std::os::unix::net::UnixStream;
        use std::path::PathBuf;
        use std::sync::{
            atomic::{AtomicBool, Ordering},
            Arc, Mutex,
        };
        use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("hitch-daemon-start-job-{nonce}"));
        std::fs::create_dir_all(&dir).unwrap();
        let store_path = dir.join("state.db");
        let config = super::DaemonConfig {
            socket_path: dir.join("daemon.sock"),
            store_path: store_path.clone(),
            managed_root: dir.join("managed"),
            hook_helper: dir.join("hook"),
            git: PathBuf::from("git"),
            gh: PathBuf::from("gh"),
            draft_provider: crate::drafts::DraftProviderConfig::from_env().unwrap(),
        };
        let store = hitch_store::Store::open(&store_path).unwrap();
        let state = Arc::new(std::sync::Mutex::new(super::DaemonState::new(
            store, config,
        )));

        let (request_writer, _request_reader) = UnixStream::pair().unwrap();
        let (peer_writer, _peer_reader) = UnixStream::pair().unwrap();
        let requester = Arc::new(super::ClientSink {
            writer: Mutex::new(super::DaemonStream::new(request_writer)),
            live: AtomicBool::new(true),
            jobs_live: AtomicBool::new(true),
            agent_state_live: AtomicBool::new(true),
            pending: Mutex::new(Vec::new()),
            pending_job_events: Mutex::new(Vec::new()),
            pending_agent_state_events: Mutex::new(Vec::new()),
        });
        let blocked = Arc::new(super::ClientSink {
            writer: Mutex::new(super::DaemonStream::new(peer_writer)),
            live: AtomicBool::new(true),
            jobs_live: AtomicBool::new(true),
            agent_state_live: AtomicBool::new(true),
            pending: Mutex::new(Vec::new()),
            pending_job_events: Mutex::new(Vec::new()),
            pending_agent_state_events: Mutex::new(Vec::new()),
        });
        {
            let mut guard = state.lock().unwrap();
            guard.clients.insert(1, requester);
            guard.clients.insert(2, Arc::clone(&blocked));
        }

        let blocked_writer = blocked.writer.lock().unwrap();
        let started = Arc::new(AtomicBool::new(false));
        let started_flag = Arc::clone(&started);
        let worker_state = Arc::clone(&state);
        let handle = std::thread::spawn(move || {
            super::start_job(
                "hitch-test-job",
                &worker_state,
                1,
                7,
                Some("push"),
                Some("Pushing…"),
                move |_, _| {
                    started_flag.store(true, Ordering::SeqCst);
                    Ok(super::Response::Ack)
                },
            )
            .unwrap();
        });

        let deadline = Instant::now() + Duration::from_millis(500);
        while !started.load(Ordering::SeqCst) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            started.load(Ordering::SeqCst),
            "worker should start even while a peer client's running-event write is blocked"
        );

        drop(blocked_writer);
        handle.join().unwrap();

        drop(state);
        let _ = std::fs::remove_dir_all(&dir);
    }
    #[test]
    fn shutdown_cancels_and_drains_active_jobs() {
        use std::path::PathBuf;
        use std::sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        };
        use std::time::{Duration, SystemTime, UNIX_EPOCH};

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("hitch-daemon-shutdown-{nonce}"));
        std::fs::create_dir_all(&dir).unwrap();
        let store_path = dir.join("state.db");
        let config = super::DaemonConfig {
            socket_path: dir.join("daemon.sock"),
            store_path: store_path.clone(),
            managed_root: dir.join("managed"),
            hook_helper: dir.join("hook"),
            git: PathBuf::from("git"),
            gh: PathBuf::from("gh"),
            draft_provider: crate::drafts::DraftProviderConfig::from_env().unwrap(),
        };
        let store = hitch_store::Store::open(&store_path).unwrap();
        let state = Arc::new(std::sync::Mutex::new(super::DaemonState::new(
            store, config,
        )));

        let job_id = hitch_core::JobId::new();
        let control = Arc::new(super::JobControl::default());
        state.lock().unwrap().jobs.insert(
            job_id,
            super::ActiveJob {
                control: Arc::clone(&control),
                kind: Some("push"),
                message: Some("Pushing…".into()),
            },
        );

        let worker_state = Arc::clone(&state);
        let worker_control = Arc::clone(&control);
        let observed_cancel = Arc::new(AtomicBool::new(false));
        let observed_cancel_clone = Arc::clone(&observed_cancel);
        let worker = std::thread::spawn(move || {
            while !worker_control.is_cancelled() {
                std::thread::sleep(Duration::from_millis(10));
            }
            observed_cancel_clone.store(true, Ordering::SeqCst);
            worker_state.lock().unwrap().jobs.remove(&job_id);
        });

        super::cancel_active_jobs(&state);
        super::wait_for_jobs_to_finish(&state);
        worker.join().unwrap();

        assert!(observed_cancel.load(Ordering::SeqCst));
        assert!(state.lock().unwrap().jobs.is_empty());

        drop(state);
        let _ = std::fs::remove_dir_all(&dir);
    }
    #[test]
    fn list_worktrees_prunes_missing_linked_worktrees() {
        use hitch_core::{Project, ProjectKind, Session, SessionParent, Worktree};
        use std::path::PathBuf;
        use std::process::Command;
        use std::sync::Arc;
        use std::time::{SystemTime, UNIX_EPOCH};

        fn run(command: &mut Command, what: &str) {
            let status = command.status().unwrap();
            assert!(status.success(), "{what} failed with {status}");
        }

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("hitch-daemon-reconcile-{nonce}"));
        std::fs::create_dir_all(&dir).unwrap();
        let store_path = dir.join("state.db");
        let managed_root = dir.join("managed");
        let project_root = dir.join("repo");
        let linked_path = dir.join("linked-feature");
        std::fs::create_dir_all(&managed_root).unwrap();

        let mut git = Command::new("git");
        git.arg("init").arg(&project_root);
        run(&mut git, "git init");

        let mut git = Command::new("git");
        git.arg("-C")
            .arg(&project_root)
            .args(["config", "user.email", "hitch@example.com"]);
        run(&mut git, "git config user.email");

        let mut git = Command::new("git");
        git.arg("-C")
            .arg(&project_root)
            .args(["config", "user.name", "Hitch Tests"]);
        run(&mut git, "git config user.name");

        let mut git = Command::new("git");
        git.arg("-C")
            .arg(&project_root)
            .args(["branch", "-M", "main"]);
        run(&mut git, "git branch -M main");

        std::fs::write(project_root.join("README.md"), "hello\n").unwrap();

        let mut git = Command::new("git");
        git.arg("-C").arg(&project_root).args(["add", "README.md"]);
        run(&mut git, "git add README.md");

        let mut git = Command::new("git");
        git.arg("-C")
            .arg(&project_root)
            .args(["commit", "-m", "initial"]);
        run(&mut git, "git commit");

        let mut git = Command::new("git");
        git.arg("-C")
            .arg(&project_root)
            .args(["worktree", "add", "-b", "feature/stale"])
            .arg(&linked_path);
        run(&mut git, "git worktree add");

        let config = super::DaemonConfig {
            socket_path: dir.join("daemon.sock"),
            store_path: store_path.clone(),
            managed_root,
            hook_helper: dir.join("hook"),
            git: PathBuf::from("git"),
            gh: PathBuf::from("gh"),
            draft_provider: crate::drafts::DraftProviderConfig::from_env().unwrap(),
        };
        let store = hitch_store::Store::open(&store_path).unwrap();
        let state = Arc::new(std::sync::Mutex::new(super::DaemonState::new(
            store, config,
        )));

        let project = Project::new("hitch", &project_root, ProjectKind::GitBacked);
        let main = Worktree::new(project.id, &project_root, "main", true, false);
        let linked = Worktree::new(project.id, &linked_path, "feature/stale", false, true);
        let session = Session::new("shell", SessionParent::Worktree(linked.id), &linked_path);
        {
            let mut guard = state.lock().unwrap();
            guard.store.insert_project(&project).unwrap();
            guard.store.insert_worktree(&main).unwrap();
            guard.store.insert_worktree(&linked).unwrap();
            guard.store.insert_session(&session).unwrap();
            guard.projects.insert(project.id, project.clone());
            guard.worktrees.insert(main.id, main.clone());
            guard.worktrees.insert(linked.id, linked.clone());
        }

        std::fs::remove_dir_all(&linked_path).unwrap();

        let listed = super::list_worktrees(&state, project.id).unwrap();
        assert_eq!(listed, vec![main.clone()]);

        let guard = state.lock().unwrap();
        assert_eq!(
            guard.store.list_worktrees(project.id).unwrap(),
            vec![main.clone()]
        );
        assert_eq!(guard.store.get_worktree(linked.id).unwrap(), None);
        assert_eq!(guard.store.get_session(session.id).unwrap(), None);
        assert!(guard.worktrees.contains_key(&main.id));
        assert!(!guard.worktrees.contains_key(&linked.id));

        drop(guard);
        drop(state);
        let _ = std::fs::remove_dir_all(&dir);
    }
    #[cfg(unix)]
    #[test]
    fn list_worktrees_broadcasts_external_branch_change() {
        use hitch_core::{Project, ProjectKind, Worktree};
        use hitch_proto::{ControlLineDecoder, ControlMessage, Event};
        use std::io::Read as _;
        use std::os::unix::net::UnixStream;
        use std::path::PathBuf;
        use std::process::Command;
        use std::sync::atomic::AtomicBool;
        use std::sync::{Arc, Mutex};
        use std::time::{Duration, SystemTime, UNIX_EPOCH};

        fn run(command: &mut Command, what: &str) {
            let status = command.status().unwrap();
            assert!(status.success(), "{what} failed with {status}");
        }

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("hitch-daemon-branch-broadcast-{nonce}"));
        std::fs::create_dir_all(&dir).unwrap();
        let store_path = dir.join("state.db");
        let managed_root = dir.join("managed");
        let project_root = dir.join("repo");
        std::fs::create_dir_all(&managed_root).unwrap();

        let mut git = Command::new("git");
        git.arg("init").arg(&project_root);
        run(&mut git, "git init");

        let mut git = Command::new("git");
        git.arg("-C")
            .arg(&project_root)
            .args(["config", "user.email", "hitch@example.com"]);
        run(&mut git, "git config user.email");

        let mut git = Command::new("git");
        git.arg("-C")
            .arg(&project_root)
            .args(["config", "user.name", "Hitch Tests"]);
        run(&mut git, "git config user.name");

        let mut git = Command::new("git");
        git.arg("-C")
            .arg(&project_root)
            .args(["branch", "-M", "main"]);
        run(&mut git, "git branch -M main");

        std::fs::write(project_root.join("README.md"), "hello\n").unwrap();

        let mut git = Command::new("git");
        git.arg("-C").arg(&project_root).args(["add", "README.md"]);
        run(&mut git, "git add README.md");

        let mut git = Command::new("git");
        git.arg("-C")
            .arg(&project_root)
            .args(["commit", "-m", "initial"]);
        run(&mut git, "git commit");

        let config = super::DaemonConfig {
            socket_path: dir.join("daemon.sock"),
            store_path: store_path.clone(),
            managed_root,
            hook_helper: dir.join("hook"),
            git: PathBuf::from("git"),
            gh: PathBuf::from("gh"),
            draft_provider: crate::drafts::DraftProviderConfig::from_env().unwrap(),
        };
        let store = hitch_store::Store::open(&store_path).unwrap();
        let state = Arc::new(std::sync::Mutex::new(super::DaemonState::new(
            store, config,
        )));

        let project = Project::new("hitch", &project_root, ProjectKind::GitBacked);
        let main = Worktree::new(project.id, &project_root, "main", true, false);
        {
            let mut guard = state.lock().unwrap();
            guard.store.insert_project(&project).unwrap();
            guard.store.insert_worktree(&main).unwrap();
            guard.projects.insert(project.id, project.clone());
            guard.worktrees.insert(main.id, main.clone());
        }

        // Register a client sink so the broadcast has a destination we can read
        // back off the wire.
        let (reader, writer) = UnixStream::pair().unwrap();
        let sink = Arc::new(super::ClientSink {
            writer: Mutex::new(super::DaemonStream::new(writer)),
            live: AtomicBool::new(true),
            jobs_live: AtomicBool::new(true),
            agent_state_live: AtomicBool::new(true),
            pending: Mutex::new(Vec::new()),
            pending_job_events: Mutex::new(Vec::new()),
            pending_agent_state_events: Mutex::new(Vec::new()),
        });
        {
            let mut guard = state.lock().unwrap();
            guard.clients.insert(1, sink);
        }

        // Switch the worktree's branch outside Hitch, the way a manual checkout
        // or an agent would.
        let mut git = Command::new("git");
        git.arg("-C")
            .arg(&project_root)
            .args(["checkout", "-b", "feature/new"]);
        run(&mut git, "git checkout -b feature/new");

        let listed = super::list_worktrees(&state, project.id).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].branch, "feature/new");

        // The external branch change must reach attached GUIs as a
        // WorktreeUpdated event — not be persisted silently — so a concurrent
        // PR refresh can't leave a fresh PR chip beside a stale branch name.
        let mut reader = reader;
        reader
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut decoder = ControlLineDecoder::new();
        let mut buf = [0u8; 8192];
        let event = loop {
            let n = reader
                .read(&mut buf)
                .expect("read worktree-updated broadcast");
            assert!(n > 0, "client sink closed before delivering the event");
            if let Some(event) = decoder
                .push(&buf[..n])
                .unwrap()
                .into_iter()
                .find_map(|message| match message {
                    ControlMessage::Event { event } => Some(event),
                    _ => None,
                })
            {
                break event;
            }
        };
        match event {
            Event::WorktreeUpdated { worktree } => {
                assert_eq!(worktree.id, main.id);
                assert_eq!(worktree.branch, "feature/new");
            }
            other => panic!("expected WorktreeUpdated event, got {other:?}"),
        }

        drop(state);
        let _ = std::fs::remove_dir_all(&dir);
    }
    #[test]
    fn remove_worktree_rejects_externally_managed_paths() {
        use hitch_core::{Project, ProjectKind, Worktree};
        use std::path::PathBuf;
        use std::sync::Arc;
        use std::time::{SystemTime, UNIX_EPOCH};

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("hitch-daemon-external-remove-{nonce}"));
        std::fs::create_dir_all(&dir).unwrap();
        let store_path = dir.join("state.db");
        let managed_root = dir.join("managed");
        let project_root = dir.join("repo");
        let external_path = managed_root.join("external").join("feature");
        std::fs::create_dir_all(&managed_root).unwrap();
        std::fs::create_dir_all(&project_root).unwrap();
        std::fs::create_dir_all(&external_path).unwrap();

        let config = super::DaemonConfig {
            socket_path: dir.join("daemon.sock"),
            store_path: store_path.clone(),
            managed_root: managed_root.clone(),
            hook_helper: dir.join("hook"),
            git: PathBuf::from("git"),
            gh: PathBuf::from("gh"),
            draft_provider: crate::drafts::DraftProviderConfig::from_env().unwrap(),
        };
        let store = hitch_store::Store::open(&store_path).unwrap();
        let state = Arc::new(std::sync::Mutex::new(super::DaemonState::new(
            store, config,
        )));

        let project = Project::new("hitch", &project_root, ProjectKind::GitBacked);
        let worktree = Worktree::new(project.id, &external_path, "feature/external", false, false);
        {
            let mut guard = state.lock().unwrap();
            guard.store.insert_project(&project).unwrap();
            guard.store.insert_worktree(&worktree).unwrap();
            guard.projects.insert(project.id, project.clone());
            guard.worktrees.insert(worktree.id, worktree.clone());
        }

        let err = super::remove_worktree(&state, worktree.id, false, false).unwrap_err();
        assert_eq!(err.code, super::ErrorCode::InvalidRequest);
        assert_eq!(
            err.message,
            "externally managed worktrees cannot be removed by Hitch"
        );
        assert!(
            external_path.exists(),
            "external worktree path must be untouched"
        );
        assert!(state.lock().unwrap().worktrees.contains_key(&worktree.id));

        drop(state);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- Slice 4: transition precedence + identity announce/replay/clear ---

    /// Build a DaemonState with one real (shell) session open under a plain
    /// project, returning the state, the session id, and the temp dir to clean
    /// up. Opening via `open_session` spawns a PTY the way production does, so
    /// the session record is the genuine `DaemonSession`.
    fn state_with_session() -> (
        std::sync::Arc<std::sync::Mutex<super::DaemonState>>,
        hitch_core::SessionId,
        std::path::PathBuf,
        std::sync::mpsc::Receiver<hitch_pty::PtyEvent>,
    ) {
        use hitch_core::{Project, ProjectKind, SessionParent};
        use std::path::PathBuf;
        use std::sync::{Arc, Mutex};
        use std::time::{SystemTime, UNIX_EPOCH};

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("hitch-daemon-slice4-{nonce}"));
        let project_root = dir.join("repo");
        std::fs::create_dir_all(&project_root).unwrap();
        let store_path = dir.join("state.db");
        let config = super::DaemonConfig {
            socket_path: dir.join("daemon.sock"),
            store_path: store_path.clone(),
            managed_root: dir.join("managed"),
            hook_helper: dir.join("hitch-hook"),
            git: PathBuf::from("git"),
            gh: PathBuf::from("gh"),
            draft_provider: crate::drafts::DraftProviderConfig::from_env().unwrap(),
        };
        let store = hitch_store::Store::open(&store_path).unwrap();
        let state = Arc::new(Mutex::new(super::DaemonState::new(store, config)));

        let project = Project::new("slice4", &project_root, ProjectKind::Plain);
        let project_id = project.id;
        {
            let mut guard = state.lock().unwrap();
            guard.store.insert_project(&project).unwrap();
            guard.projects.insert(project.id, project);
        }

        // Keep the receiver alive so the PTY reader thread's sends don't error;
        // the tests never need to consume it.
        let (pty_tx, pty_rx) = std::sync::mpsc::channel();
        let session = super::open_session(
            &state,
            SessionParent::Project(project_id),
            "shell".into(),
            None,
            80,
            24,
            &pty_tx,
        )
        .unwrap();
        (state, session.id, dir, pty_rx)
    }

    fn current_state(
        state: &std::sync::Arc<std::sync::Mutex<super::DaemonState>>,
        session_id: hitch_core::SessionId,
    ) -> (
        Option<hitch_proto::KnownAgent>,
        Option<hitch_core::AgentState>,
    ) {
        let guard = state.lock().unwrap();
        let s = guard.sessions.get(&session_id).unwrap();
        (s.agent, s.agent_state)
    }

    #[test]
    fn waiting_report_does_not_downgrade_error() {
        use hitch_core::AgentState;
        use hitch_proto::KnownAgent;

        let (state, session_id, dir, _rx) = state_with_session();

        // Enter running (clears the late-arrival guard), then error.
        super::store_agent_report(
            &state,
            KnownAgent::ClaudeCode,
            Some(AgentState::Running),
            Some(session_id),
            None,
            None,
        )
        .unwrap();
        super::store_agent_report(
            &state,
            KnownAgent::ClaudeCode,
            Some(AgentState::Error),
            Some(session_id),
            None,
            Some("rate limited".into()),
        )
        .unwrap();

        // The idle-prompt heal (waiting) must NOT downgrade error.
        let event = super::store_agent_report(
            &state,
            KnownAgent::ClaudeCode,
            Some(AgentState::Waiting),
            Some(session_id),
            None,
            None,
        )
        .unwrap();
        assert!(event.is_none(), "waiting over error must not broadcast");
        assert_eq!(
            current_state(&state, session_id),
            (Some(KnownAgent::ClaudeCode), Some(AgentState::Error)),
            "error must hold through the idle heal"
        );

        drop(state);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn running_report_clears_error() {
        use hitch_core::AgentState;
        use hitch_proto::KnownAgent;

        let (state, session_id, dir, _rx) = state_with_session();

        for s in [AgentState::Running, AgentState::Error] {
            super::store_agent_report(
                &state,
                KnownAgent::ClaudeCode,
                Some(s),
                Some(session_id),
                None,
                None,
            )
            .unwrap();
        }

        // UserPromptSubmit → running clears error.
        let event = super::store_agent_report(
            &state,
            KnownAgent::ClaudeCode,
            Some(AgentState::Running),
            Some(session_id),
            None,
            None,
        )
        .unwrap();
        assert!(event.is_some(), "running over error must broadcast");
        assert_eq!(
            current_state(&state, session_id),
            (Some(KnownAgent::ClaudeCode), Some(AgentState::Running)),
        );

        drop(state);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn exit_to_none_clears_error_and_identity() {
        use hitch_core::AgentState;
        use hitch_proto::KnownAgent;

        let (state, session_id, dir, _rx) = state_with_session();

        for s in [AgentState::Running, AgentState::Error] {
            super::store_agent_report(
                &state,
                KnownAgent::ClaudeCode,
                Some(s),
                Some(session_id),
                None,
                None,
            )
            .unwrap();
        }

        // Exit-to-None clears both state and identity (mark reverts to shell).
        let event = super::store_agent_report(
            &state,
            KnownAgent::ClaudeCode,
            None,
            Some(session_id),
            None,
            None,
        )
        .unwrap();
        assert!(event.is_some(), "exit-to-None must broadcast a clear");
        assert_eq!(current_state(&state, session_id), (None, None));

        drop(state);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn announce_stores_identity_pre_running_without_tripping_guard() {
        use hitch_core::AgentState;
        use hitch_proto::KnownAgent;

        let (state, session_id, dir, _rx) = state_with_session();

        // A fresh, never-prompted session: the late-arrival guard is off here
        // (set true only after a clear), but the announce must store identity
        // while leaving Agent State absent.
        let event =
            super::store_agent_announce(&state, KnownAgent::ClaudeCode, Some(session_id), None)
                .unwrap();
        assert!(event.is_some(), "a new identity must broadcast");
        assert_eq!(
            current_state(&state, session_id),
            (Some(KnownAgent::ClaudeCode), None),
            "announce sets identity but never sets state"
        );

        // Announce must bypass the late-arrival guard: simulate a guard that is
        // closed (as it is right after an exit-to-None clear) and confirm an
        // announce still stores identity even though no `running` has arrived.
        {
            let mut guard = state.lock().unwrap();
            let s = guard.sessions.get_mut(&session_id).unwrap();
            s.agent = None;
            s.agent_report_requires_running = true;
        }
        let event =
            super::store_agent_announce(&state, KnownAgent::Codex, Some(session_id), None).unwrap();
        assert!(event.is_some(), "announce must pass the late-arrival guard");
        assert_eq!(
            current_state(&state, session_id),
            (Some(KnownAgent::Codex), None),
            "announce stores identity even while the state guard is closed"
        );
        // A *state* report (non-running) is still dropped by the guard.
        let dropped = super::store_agent_report(
            &state,
            KnownAgent::Codex,
            Some(AgentState::Waiting),
            Some(session_id),
            None,
            None,
        )
        .unwrap();
        assert!(
            dropped.is_none(),
            "state guard still drops late non-running"
        );
        assert_eq!(
            current_state(&state, session_id),
            (Some(KnownAgent::Codex), None),
        );

        drop(state);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn exit_clear_passes_the_late_arrival_guard() {
        use hitch_core::AgentState;
        use hitch_proto::KnownAgent;

        let (state, session_id, dir, _rx) = state_with_session();

        // Run an agent to completion once: the exit-to-None clear closes the
        // late-arrival guard.
        super::store_agent_report(
            &state,
            KnownAgent::ClaudeCode,
            Some(AgentState::Running),
            Some(session_id),
            None,
            None,
        )
        .unwrap();
        super::store_agent_report(
            &state,
            KnownAgent::ClaudeCode,
            None,
            Some(session_id),
            None,
            None,
        )
        .unwrap();

        // Second run: announce only (the user opens the TUI but never prompts).
        super::store_agent_announce(&state, KnownAgent::ClaudeCode, Some(session_id), None)
            .unwrap();
        assert_eq!(
            current_state(&state, session_id),
            (Some(KnownAgent::ClaudeCode), None),
        );

        // Exit without ever prompting: the SessionEnd clear must pass the guard
        // and clear the announced identity, or the Session mark sticks forever
        // (the guard exists to drop stale *state* reports, never clears — a
        // clear is idempotent and is the only thing that can revert the mark
        // for a never-prompted agent).
        let event = super::store_agent_report(
            &state,
            KnownAgent::ClaudeCode,
            None,
            Some(session_id),
            None,
            None,
        )
        .unwrap();
        assert!(
            event.is_some(),
            "the exit clear must broadcast past the guard"
        );
        let event = event.unwrap();
        assert_eq!(event.agent, None, "the clear must carry agent: None");
        assert_eq!(current_state(&state, session_id), (None, None));

        drop(state);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn backstop_clears_announced_identity_without_state() {
        use hitch_proto::KnownAgent;

        let (state, session_id, dir, _rx) = state_with_session();

        // Announce only — a never-prompted agent has identity but no state.
        super::store_agent_announce(&state, KnownAgent::ClaudeCode, Some(session_id), None)
            .unwrap();

        // The agent dies without SessionEnd (dirty exit); the foreground
        // command poller sees the shell again. The backstop must clear the
        // announced identity even though there is no Agent State to clear.
        let event = super::clear_stale_agent_state(&state, session_id, Some("zsh"));
        assert!(
            event.is_some(),
            "the backstop must clear an identity-only session"
        );
        let event = event.unwrap();
        assert_eq!(event.agent, None);
        assert_eq!(current_state(&state, session_id), (None, None));

        drop(state);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn session_opened_replay_uses_current_agent_state_and_output_gate() {
        use hitch_core::AgentState;
        use hitch_proto::KnownAgent;
        use std::time::Instant;

        let (state, session_id, dir, _rx) = state_with_session();

        super::store_agent_report(
            &state,
            KnownAgent::ClaudeCode,
            Some(AgentState::Running),
            Some(session_id),
            None,
            Some("busy".into()),
        )
        .unwrap();
        {
            let mut guard = state.lock().unwrap();
            super::mark_output_active(&mut guard, session_id, Instant::now());
        }

        let replay = super::session_opened_replay(&state, session_id).unwrap();
        assert_eq!(replay.agent, Some(KnownAgent::ClaudeCode));
        assert_eq!(replay.agent_state, Some(AgentState::Running));
        assert_eq!(replay.agent_detail.as_deref(), Some("busy"));
        assert!(replay.output_active);

        drop(state);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn announced_identity_is_replayed_in_session_opened() {
        use hitch_proto::KnownAgent;

        let (state, session_id, dir, _rx) = state_with_session();

        super::store_agent_announce(&state, KnownAgent::ClaudeCode, Some(session_id), None)
            .unwrap();

        // Replay reads identity straight off the session record (the same field
        // `attach`'s SessionOpened replay reads). Confirm it is populated even
        // with no Agent State yet.
        let (agent, agent_state) = {
            let guard = state.lock().unwrap();
            let s = guard.sessions.get(&session_id).unwrap();
            (s.agent, s.agent_state)
        };
        assert_eq!(agent, Some(KnownAgent::ClaudeCode));
        assert_eq!(agent_state, None);

        drop(state);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn announce_is_idempotent_for_same_identity() {
        use hitch_proto::KnownAgent;

        let (state, session_id, dir, _rx) = state_with_session();

        let first =
            super::store_agent_announce(&state, KnownAgent::ClaudeCode, Some(session_id), None)
                .unwrap();
        assert!(first.is_some());
        // Re-announcing the same agent is a no-op: no redundant broadcast.
        let second =
            super::store_agent_announce(&state, KnownAgent::ClaudeCode, Some(session_id), None)
                .unwrap();
        assert!(second.is_none(), "unchanged identity must not re-broadcast");

        drop(state);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn gate_is_active(
        state: &std::sync::Arc<std::sync::Mutex<super::DaemonState>>,
        session_id: hitch_core::SessionId,
    ) -> bool {
        state
            .lock()
            .unwrap()
            .sessions
            .get(&session_id)
            .unwrap()
            .output_active
    }

    #[test]
    fn output_first_frame_rises_then_stays_active_without_per_frame_broadcast() {
        use std::time::{Duration, Instant};

        let (state, session_id, dir, _rx) = state_with_session();
        let t0 = Instant::now();

        // First frame after a quiet period is the rising edge: it broadcasts.
        let edge = {
            let mut guard = state.lock().unwrap();
            super::mark_output_active(&mut guard, session_id, t0)
        };
        match edge {
            Some(super::Event::OutputActive { active, .. }) => assert!(active),
            other => panic!("expected a rising-edge active event, got {other:?}"),
        }
        assert!(gate_is_active(&state, session_id));

        // A second frame while already active must NOT broadcast (no spam), even
        // though it still refreshes the last-output instant.
        let edge = {
            let mut guard = state.lock().unwrap();
            super::mark_output_active(&mut guard, session_id, t0 + Duration::from_millis(10))
        };
        assert!(
            edge.is_none(),
            "no per-frame broadcast while already active"
        );
        assert!(gate_is_active(&state, session_id));

        drop(state);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn output_falls_inactive_after_quiet_period() {
        use std::time::{Duration, Instant};

        let (state, session_id, dir, _rx) = state_with_session();
        let quiet = Duration::from_secs(4);
        let t0 = Instant::now();

        // Go active.
        {
            let mut guard = state.lock().unwrap();
            super::mark_output_active(&mut guard, session_id, t0);
        }

        // Before the quiet window elapses there is no falling edge.
        let edges = {
            let mut guard = state.lock().unwrap();
            super::collect_output_quiet_edges(&mut guard, t0 + Duration::from_secs(3), quiet)
        };
        assert!(edges.is_empty(), "must stay active before the quiet window");
        assert!(gate_is_active(&state, session_id));

        // Once quiet for >= N, the falling edge fires exactly once.
        let edges = {
            let mut guard = state.lock().unwrap();
            super::collect_output_quiet_edges(&mut guard, t0 + quiet, quiet)
        };
        assert_eq!(edges.len(), 1, "exactly one falling edge");
        match &edges[0] {
            super::Event::OutputActive {
                session_id: id,
                active,
                ..
            } => {
                assert_eq!(*id, session_id);
                assert!(!*active, "falling edge carries active: false");
            }
            other => panic!("expected a falling-edge event, got {other:?}"),
        }
        assert!(!gate_is_active(&state, session_id));

        // Idempotent: an already-inactive session never re-fires the falling edge.
        let edges = {
            let mut guard = state.lock().unwrap();
            super::collect_output_quiet_edges(&mut guard, t0 + quiet + quiet, quiet)
        };
        assert!(edges.is_empty(), "no repeat falling edge once inactive");

        drop(state);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn output_reactivates_after_quiet_period() {
        use std::time::{Duration, Instant};

        let (state, session_id, dir, _rx) = state_with_session();
        let quiet = Duration::from_secs(4);
        let t0 = Instant::now();

        // Active, then quiet → inactive.
        {
            let mut guard = state.lock().unwrap();
            super::mark_output_active(&mut guard, session_id, t0);
        }
        {
            let mut guard = state.lock().unwrap();
            super::collect_output_quiet_edges(&mut guard, t0 + quiet, quiet);
        }
        assert!(!gate_is_active(&state, session_id));

        // New output after the quiet period broadcasts a fresh rising edge.
        let edge = {
            let mut guard = state.lock().unwrap();
            super::mark_output_active(&mut guard, session_id, t0 + quiet + Duration::from_secs(1))
        };
        match edge {
            Some(super::Event::OutputActive { active, .. }) => {
                assert!(active, "reactivation re-broadcasts active: true")
            }
            other => panic!("expected a rising-edge event on reactivation, got {other:?}"),
        }
        assert!(gate_is_active(&state, session_id));

        drop(state);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn output_gate_starts_inactive_and_quiet_scan_ignores_never_active_sessions() {
        use std::time::{Duration, Instant};

        let (state, session_id, dir, _rx) = state_with_session();

        // A brand-new session has produced no output: the gate is inactive and a
        // quiet scan emits nothing (no last_output_at to measure against).
        assert!(!gate_is_active(&state, session_id));
        let edges = {
            let mut guard = state.lock().unwrap();
            super::collect_output_quiet_edges(
                &mut guard,
                Instant::now() + Duration::from_secs(60),
                Duration::from_secs(4),
            )
        };
        assert!(
            edges.is_empty(),
            "never-active session yields no falling edge"
        );

        drop(state);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
