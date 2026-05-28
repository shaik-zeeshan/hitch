//! `hitch-daemon` — the long-lived process that owns every PTY, buffers
//! scrollback, persists layout, and serves the `hitch-proto` socket (ADR 0003).
//!
//! Slice 7 daemon composition: this binary is the sole composer of the feature
//! crates (ADR 0005). It wires store + git + PTY + agent-hook installation into
//! the socket API consumed by the desktop client and `hitch-hook`.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::fs::FileTypeExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;

use hitch_agent::HookInstallOptions;
use hitch_core::{
    Project, ProjectId, ProjectKind, Session, SessionId, SessionParent, Worktree, WorktreeId,
};
use hitch_git::{
    CreatePrRequest, CreateWorktreeRequest, DiffTarget, FileState, GitClient, GitRepository,
    RemoveWorktreeRequest, StatusEntry, WorktreeCheckout,
};
use hitch_proto::{
    encode_control_message, encode_pty_frame, ChangedFile, ControlMessage, ErrorCode, Event,
    FileDiff, FileStatus, GitStatus, ProtocolError, Request, Response, WorktreeCreateMode,
    MAX_PTY_FRAME_LEN, PROTOCOL_VERSION,
};
use hitch_pty::{
    ManagedPty, PtyEvent, PtySpawnConfig, TerminalSize, DEFAULT_SCROLLBACK_CAPACITY,
};
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

#[derive(Debug)]
struct Args {
    socket_path: PathBuf,
    store_path: PathBuf,
    managed_root: PathBuf,
    hook_helper: PathBuf,
    git: PathBuf,
    gh: PathBuf,
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
                "--detach" => detach = true,
                "--help" | "-h" => {
                    println!(
                        "usage: hitch-daemon [--socket PATH] [--store PATH] [--managed-root PATH] [--hook-helper PATH] [--git PATH] [--gh PATH] [--detach]"
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
        }
    }
}

fn detach_spawn(args: &Args) -> io::Result<()> {
    let exe = std::env::current_exe()?;
    let child = Command::new(exe)
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
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    println!("{}", child.id());
    Ok(())
}

fn run_daemon(config: DaemonConfig) -> io::Result<()> {
    remove_stale_socket(&config.socket_path)?;
    if let Some(parent) = config.socket_path.parent() {
        fs::create_dir_all(parent)?;
    }
    if let Some(parent) = config.store_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::create_dir_all(&config.managed_root)?;

    let listener = UnixListener::bind(&config.socket_path)?;
    listener.set_nonblocking(true)?;

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
    spawn_command_poller(Arc::clone(&state), Arc::clone(&shutdown));
    spawn_dirty_poller(Arc::clone(&state), Arc::clone(&shutdown));

    while !shutdown.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, _)) => {
                stream.set_nonblocking(false)?;
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
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(25));
            }
            Err(err) => return Err(err),
        }
    }

    kill_all_sessions(&state);
    let _ = fs::remove_file(config.socket_path);
    Ok(())
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
        }
    }
}

struct DaemonSession {
    session: Session,
    pty: Arc<ManagedPty>,
    restored_scrollback: Vec<u8>,
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
    /// head so the log never exceeds `DEFAULT_SCROLLBACK_CAPACITY`. `_restored`
    /// is accepted for symmetry with `replay_snapshot` (which composes it) but is
    /// not stored here — the log holds live bytes only, matching the reader ring.
    fn record_output(&mut self, session_id: SessionId, _restored: &[u8], bytes: &[u8]) {
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
    writer: Mutex<UnixStream>,
    /// Output readiness gate. A freshly accepted client is registered in
    /// `DaemonState.clients` at accept time (so control-plane broadcasts reach
    /// it immediately), but it must NOT receive live `SessionOutput` until the
    /// dispatcher has replayed each session's scrollback to it on the dispatcher
    /// thread. `false` until that replay completes; the output broadcast path
    /// skips any sink whose gate is still closed. Non-output broadcasts ignore
    /// this flag — the desktop upserts those idempotently (ADR 0007), so they
    /// are not the lossy path and must keep flowing.
    live: AtomicBool,
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
        let restored_scrollback = {
            let state = state.lock().map_err(|_| internal("state lock poisoned"))?;
            state
                .store
                .load_scrollback(session.id)
                .map_err(store_error)?
                .unwrap_or_default()
        };
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
                restored_scrollback,
            },
        );
    }
    Ok(())
}

fn register_client(state: &Arc<Mutex<DaemonState>>, stream: &UnixStream) -> io::Result<u64> {
    let writer = stream.try_clone()?;
    let mut state = state.lock().map_err(|_| poisoned("state"))?;
    let client_id = state.next_client_id;
    state.next_client_id += 1;
    state.clients.insert(
        client_id,
        Arc::new(ClientSink {
            writer: Mutex::new(writer),
            // Closed until the dispatcher replays scrollback to this client.
            live: AtomicBool::new(false),
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
    stream: UnixStream,
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
        Request::CloneProject {
            remote_url,
            destination,
            name,
        } => {
            let root = clone_project(&remote_url, &destination, name.as_deref())?;
            let project = add_project_from_root(state, &root, name.as_deref())?;
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
        Request::ListWorktrees { project_id } => {
            let worktrees = list_worktrees(state, project_id)?;
            send_response(
                state,
                client_id,
                request_id,
                Response::Worktrees { worktrees },
            )?;
        }
        Request::CreateWorktree {
            project_id,
            branch,
            base,
            mode,
        } => {
            let worktree = create_worktree(state, project_id, branch, base, mode)?;
            send_response(
                state,
                client_id,
                request_id,
                Response::Worktrees {
                    worktrees: vec![worktree.clone()],
                },
            )?;
            broadcast_event(state, Event::WorktreeUpdated { worktree })?;
        }
        Request::RemoveWorktree {
            worktree_id,
            delete_branch,
            force,
        } => {
            remove_worktree(state, worktree_id, delete_branch, force)?;
            send_response(state, client_id, request_id, Response::Ack)?;
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
            let session =
                open_session(state, parent, name, command, cols, rows, &channels.pty_tx)?;
            send_response(
                state,
                client_id,
                request_id,
                Response::SessionOpened {
                    session: session.clone(),
                },
            )?;
            broadcast_event(state, Event::SessionOpened { session })?;
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
            message,
        } => {
            let (git, worktree_path) = git_context(state, worktree_id)?;
            git.commit(&worktree_path, &message).map_err(git_error)?;
            send_response(state, client_id, request_id, Response::Ack)?;
            broadcast_dirty(state, worktree_id)?;
        }
        Request::Push { worktree_id } => {
            let (git, worktree) = refreshed_worktree_context(state, worktree_id)?;
            git.push(&worktree.path, "origin", &worktree.branch, true)
                .map_err(git_error)?;
            send_response(state, client_id, request_id, Response::Ack)?;
        }
        Request::CreatePullRequest {
            worktree_id,
            title,
            body,
            base,
            draft,
        } => {
            let (git, worktree) = refreshed_worktree_context(state, worktree_id)?;
            let url = git
                .create_pr(
                    &worktree.path,
                    &CreatePrRequest {
                        title,
                        body,
                        base,
                        head: Some(worktree.branch),
                        remote: None,
                        draft,
                    },
                )
                .map_err(git_error)?;
            send_response(
                state,
                client_id,
                request_id,
                Response::PullRequestCreated { url },
            )?;
        }
        Request::InstallAgentHooks { worktree_id } => {
            let (path, helper) = {
                let state = state.lock().map_err(|_| internal("state lock poisoned"))?;
                let worktree = state
                    .worktrees
                    .get(&worktree_id)
                    .ok_or_else(|| ProtocolError::new(ErrorCode::NotFound, "worktree not found"))?;
                (worktree.path.clone(), state.config.hook_helper.clone())
            };
            hitch_agent::install_hooks(&path, &HookInstallOptions::new(helper))
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
            let (matched_session_id, worktree_id) =
                resolve_agent_target(state, session_id, cwd.as_deref())?;
            broadcast_event(
                state,
                Event::AgentState {
                    session_id: matched_session_id,
                    worktree_id,
                    agent,
                    state: agent_state,
                    detail,
                },
            )?;
            send_response(state, client_id, request_id, Response::Ack)?;
        }
        Request::ShutdownDaemon => {
            send_response(state, client_id, request_id, Response::Ack)?;
            shutdown.store(true, Ordering::SeqCst);
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

    let main_worktree = branch.map(|branch| Worktree::new(project.id, project_root, branch, true));
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
    let worktrees = {
        let state = state.lock().map_err(|_| internal("state lock poisoned"))?;
        state
            .store
            .list_worktrees(project_id)
            .map_err(store_error)?
    };

    worktrees
        .into_iter()
        .map(|worktree| {
            refresh_worktree_branch_from_disk(state, worktree).map(|(worktree, _)| worktree)
        })
        .collect()
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
    remote_url: &str,
    destination: &Path,
    name: Option<&str>,
) -> Result<PathBuf, ProtocolError> {
    let target = match name {
        Some(name) => destination.join(name),
        None => destination.to_path_buf(),
    };
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| ProtocolError::new(ErrorCode::InvalidRequest, err.to_string()))?;
    }
    let output = Command::new("git")
        .arg("clone")
        .arg(remote_url)
        .arg(&target)
        .output()
        .map_err(|err| ProtocolError::new(ErrorCode::GitFailed, err.to_string()))?;
    if !output.status.success() {
        return Err(ProtocolError::new(
            ErrorCode::GitFailed,
            String::from_utf8_lossy(&output.stderr).into_owned(),
        ));
    }
    Ok(target)
}

fn create_worktree(
    state: &Arc<Mutex<DaemonState>>,
    project_id: ProjectId,
    branch: String,
    base: Option<String>,
    mode: WorktreeCreateMode,
) -> Result<Worktree, ProtocolError> {
    let (project, managed_root, git) = {
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
        )
    };
    if project.kind != ProjectKind::GitBacked {
        return Err(ProtocolError::new(
            ErrorCode::InvalidRequest,
            "plain projects do not support worktrees",
        ));
    }

    let worktree = git
        .create_worktree(
            &project.root,
            &CreateWorktreeRequest {
                project_id,
                project_name: project.name,
                managed_root,
                branch,
                checkout: match mode {
                    WorktreeCreateMode::NewBranch => WorktreeCheckout::NewBranch,
                    WorktreeCreateMode::ExistingBranch => WorktreeCheckout::ExistingBranch,
                },
                base,
            },
        )
        .map_err(git_error)?;
    let mut state = state.lock().map_err(|_| internal("state lock poisoned"))?;
    state
        .store
        .insert_worktree(&worktree)
        .map_err(store_error)?;
    state.worktrees.insert(worktree.id, worktree.clone());
    Ok(worktree)
}

fn remove_worktree(
    state: &Arc<Mutex<DaemonState>>,
    worktree_id: WorktreeId,
    delete_branch: bool,
    force: bool,
) -> Result<(), ProtocolError> {
    let (project, worktree, git, live_session_ids) = {
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
        (project, worktree, state.git.clone(), live_session_ids)
    };

    if worktree.is_main {
        return Err(ProtocolError::new(
            ErrorCode::InvalidRequest,
            "main worktree cannot be removed by Hitch",
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

    for session_id in live_session_ids {
        close_session(state, session_id, true)?;
    }

    git.remove_worktree(
        &project.root,
        &RemoveWorktreeRequest {
            path: worktree.path,
            force,
            delete_branch: delete_branch.then_some(worktree.branch),
        },
    )
    .map_err(git_error)?;

    let mut state = state.lock().map_err(|_| internal("state lock poisoned"))?;
    state
        .store
        .delete_worktree(worktree_id)
        .map_err(store_error)?;
    state.worktrees.remove(&worktree_id);
    Ok(())
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
        },
    );
    Ok(session)
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
    let summary = GitRepository::discover(&worktree.path)
        .map_err(git_error)?
        .status()
        .map_err(git_error)?;
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
        ahead: 0,
        behind: 0,
        additions: summary.additions.min(u32::MAX as usize) as u32,
        deletions: summary.deletions.min(u32::MAX as usize) as u32,
        files: summary.entries.iter().map(status_entry_to_proto).collect(),
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
    let mut diff = repo
        .diff_file(&path, DiffTarget::Worktree)
        .map_err(git_error)?;
    if diff.is_empty() {
        diff = repo
            .diff_file(&path, DiffTarget::Staged)
            .map_err(git_error)?;
    }
    Ok(FileDiff {
        worktree_id,
        path,
        diff,
    })
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

fn status_entry_to_proto(entry: &StatusEntry) -> ChangedFile {
    let staged = entry.index != FileState::Unmodified;
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
/// keeps idle terminals quiet.
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
                                command,
                            },
                        );
                    }
                }
                thread::sleep(Duration::from_millis(1000));
            }
        })
        .expect("failed to spawn command poller thread");
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
                        persist_scrollback(&state, session_id);
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
    if let Ok(mut state) = state.lock() {
        // Compose-at-replay-time: the log holds live bytes only; restored
        // scrollback is prepended in `replay_snapshot`. Pass it for API symmetry.
        let restored = state
            .sessions
            .get(&session_id)
            .map(|session| session.restored_scrollback.clone())
            .unwrap_or_default();
        state
            .broadcaster
            .record_output(session_id, &restored, bytes);
    }
    let _ = broadcast_session_output(state, session_id, bytes);
}

fn persist_scrollback(state: &Arc<Mutex<DaemonState>>, session_id: SessionId) {
    if let Ok(state) = state.lock() {
        if let Some(session) = state.sessions.get(&session_id) {
            let mut bytes = session.restored_scrollback.clone();
            bytes.extend(session.pty.scrollback());
            let _ = state.store.save_scrollback(session_id, &bytes);
        }
    }
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

fn resolve_agent_target(
    state: &Arc<Mutex<DaemonState>>,
    session_id: Option<SessionId>,
    cwd: Option<&Path>,
) -> Result<(Option<SessionId>, Option<WorktreeId>), ProtocolError> {
    let state = state.lock().map_err(|_| internal("state lock poisoned"))?;
    let matched_session = session_id
        .and_then(|id| state.sessions.get(&id).map(|session| (id, session)))
        .or_else(|| {
            cwd.and_then(|cwd| {
                state
                    .sessions
                    .iter()
                    .find(|(_, session)| session.session.cwd == cwd)
                    .map(|(id, session)| (*id, session))
            })
        });

    let Some((session_id, daemon_session)) = matched_session else {
        return Ok((session_id, None));
    };

    let worktree_id = match daemon_session.session.parent {
        SessionParent::Worktree(worktree_id) => Some(worktree_id),
        SessionParent::Project(_) => None,
    };
    Ok((Some(session_id), worktree_id))
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
    let replay_items = {
        let state = state.lock().map_err(|_| internal("state lock poisoned"))?;
        state
            .sessions
            .values()
            .map(|daemon_session| {
                let scrollback = state.broadcaster.replay_snapshot(
                    daemon_session.session.id,
                    &daemon_session.restored_scrollback,
                );
                let command = daemon_session.pty.foreground_command();
                (daemon_session.session.clone(), scrollback, command)
            })
            .collect::<Vec<_>>()
    };

    for (session, scrollback, command) in replay_items {
        send_event_to_client(
            state,
            client_id,
            Event::SessionOpened {
                session: session.clone(),
            },
        )?;
        send_event_to_client(
            state,
            client_id,
            Event::SessionCommand {
                session_id: session.id,
                command,
            },
        )?;
        if !scrollback.is_empty() {
            send_output_to_client(state, client_id, session.id, &scrollback)?;
        }
    }

    // Snapshot fully delivered: open the gate so the dispatcher's next Output
    // broadcasts reach this client. Setting it on the dispatcher thread, after
    // the loop, guarantees no live output preceded the snapshot. The broadcast
    // path reads the sink's `live` atomic; we also mirror the gate in the
    // broadcaster's set so the unit-tested core stays a self-contained model of
    // the same state (and `forget_client` can reason about it).
    if let Ok(mut state) = state.lock() {
        if let Some(sink) = state.clients.get(&client_id) {
            sink.live.store(true, Ordering::SeqCst);
        }
        state.broadcaster.mark_live(client_id);
    }
    Ok(())
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

fn broadcast_session_output(
    state: &Arc<Mutex<DaemonState>>,
    session_id: SessionId,
    bytes: &[u8],
) -> Result<(), ProtocolError> {
    // Gate on the per-client readiness flag: a client registered at accept time
    // but not yet replayed has `live == false`, and must NOT receive live output
    // before its `SessionOpened` replay (which the desktop uses to reset its byte
    // ring). Skipping it here, on the dispatcher thread, is the second half of the
    // fix — the dispatcher both opens the gate (during replay) and reads it (here),
    // so a now-live client only ever sees output broadcast strictly after its
    // snapshot. Control-plane broadcasts deliberately do NOT gate (see
    // `broadcast_event`); the desktop upserts those idempotently (ADR 0007).
    broadcast_with_filter(
        state,
        |sink| sink.live.load(Ordering::SeqCst),
        |sink| write_output_to_sink(sink, session_id, bytes),
    );
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

fn default_store_path() -> PathBuf {
    home_dir().join(".hitch/hitch.sqlite")
}

fn default_managed_worktree_root() -> PathBuf {
    home_dir().join(".hitch/worktrees")
}

fn default_hook_helper_path() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(|parent| parent.join("hitch-hook")))
        .unwrap_or_else(|| PathBuf::from("hitch-hook"))
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
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
    use super::OutputBroadcaster;
    use hitch_core::SessionId;

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
            self.broadcaster
                .record_output(self.session_id, &self.restored, bytes);
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
}
