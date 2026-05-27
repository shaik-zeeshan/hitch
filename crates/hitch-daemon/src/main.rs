//! `hitch-daemon` — the long-lived process that owns every PTY, buffers
//! scrollback, persists layout, and serves the `hitch-proto` socket (ADR 0003).
//!
//! Slice 7 daemon composition: this binary is the sole composer of the feature
//! crates (ADR 0005). It wires store + git + PTY + agent-hook installation into
//! the socket API consumed by the desktop client and `hitch-hook`.

use std::collections::HashMap;
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

mod drafts;

use drafts::{CommitDraftInput, DraftProviderConfig, PullRequestDraftInput};
use hitch_agent::HookInstallOptions;
use hitch_core::{
    Project, ProjectId, ProjectKind, Session, SessionId, SessionParent, Worktree, WorktreeId,
};
use hitch_git::{
    staged_diff, CreatePrRequest, CreateWorktreeRequest, DiffTarget, FileState, GitClient,
    GitRepository, RemoveWorktreeRequest, StatusEntry, WorktreeCheckout,
};
use hitch_proto::{
    encode_control_message, encode_pty_frame, ChangedFile, CommitDraft, ControlMessage,
    DraftGenerationSettings, DraftProvider, ErrorCode, Event, FileDiff, FileStatus, GitStatus,
    ProtocolError, PullRequestDraft, Request, Response, WorktreeCreateMode, MAX_PTY_FRAME_LEN,
    PROTOCOL_VERSION,
};
use hitch_pty::{ManagedPty, PtyEvent, PtySpawnConfig, TerminalSize};
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
    let child = child
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
    let (pty_tx, pty_rx) = mpsc::channel::<PtyEvent>();
    let store = Store::open(&config.store_path).map_err(io::Error::other)?;
    let state = Arc::new(Mutex::new(DaemonState::new(store, config.clone())));

    restore_layout(&state, &pty_tx).map_err(|err| io::Error::other(err.message))?;
    spawn_pty_dispatcher(Arc::clone(&state), pty_rx, Arc::clone(&shutdown));
    spawn_command_poller(Arc::clone(&state), Arc::clone(&shutdown));
    spawn_dirty_poller(Arc::clone(&state), Arc::clone(&shutdown));

    while !shutdown.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, _)) => {
                stream.set_nonblocking(false)?;
                let client_id = register_client(&state, &stream)?;
                let state = Arc::clone(&state);
                let shutdown = Arc::clone(&shutdown);
                let pty_tx = pty_tx.clone();
                thread::Builder::new()
                    .name(format!("hitch-client-{client_id}"))
                    .spawn(move || handle_client(client_id, stream, state, shutdown, pty_tx))
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
        }
    }
}

struct DaemonSession {
    session: Session,
    pty: Arc<ManagedPty>,
    restored_scrollback: Vec<u8>,
}

struct ClientSink {
    writer: Mutex<UnixStream>,
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
        }),
    );
    Ok(client_id)
}

fn unregister_client(state: &Arc<Mutex<DaemonState>>, client_id: u64) {
    if let Ok(mut state) = state.lock() {
        state.clients.remove(&client_id);
    }
}

fn handle_client(
    client_id: u64,
    stream: UnixStream,
    state: Arc<Mutex<DaemonState>>,
    shutdown: Arc<AtomicBool>,
    pty_tx: mpsc::Sender<PtyEvent>,
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
            &pty_tx,
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
    pty_tx: &mpsc::Sender<PtyEvent>,
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
            replay_sessions_to_client(state, client_id)?;
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
        Request::RemoveProject { project_id, force } => {
            remove_project(state, project_id, force)?;
            send_response(state, client_id, request_id, Response::Ack)?;
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
        } => {
            let session = open_session(state, parent, name, command, pty_tx)?;
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
            subject,
            body,
        } => {
            let (git, worktree_path) = git_context(state, worktree_id)?;
            git.commit(&worktree_path, &subject, body.as_deref())
                .map_err(git_error)?;
            send_response(state, client_id, request_id, Response::Ack)?;
            broadcast_dirty(state, worktree_id)?;
        }
        Request::ListDraftModels { provider } => {
            spawn_response_task(
                "hitch-draft-models",
                Arc::clone(state),
                client_id,
                request_id,
                move |state| {
                    let models = list_draft_models(state, provider)?;
                    Ok(Response::DraftModels { provider, models })
                },
            )?;
        }
        Request::GenerateCommitDraft {
            worktree_id,
            settings,
        } => {
            spawn_response_task(
                "hitch-commit-draft",
                Arc::clone(state),
                client_id,
                request_id,
                move |state| {
                    let draft = generate_commit_draft(state, worktree_id, settings)?;
                    Ok(Response::CommitDraft { draft })
                },
            )?;
        }
        Request::GeneratePullRequestDraft {
            worktree_id,
            base,
            settings,
        } => {
            spawn_response_task(
                "hitch-pr-draft",
                Arc::clone(state),
                client_id,
                request_id,
                move |state| {
                    let draft = generate_pull_request_draft(state, worktree_id, base, settings)?;
                    Ok(Response::PullRequestDraft { draft })
                },
            )?;
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

fn remove_project(
    state: &Arc<Mutex<DaemonState>>,
    project_id: ProjectId,
    force: bool,
) -> Result<(), ProtocolError> {
    let (worktree_ids, live_session_ids) = {
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

    for session_id in live_session_ids {
        close_session(state, session_id, true)?;
    }

    let mut state = state.lock().map_err(|_| internal("state lock poisoned"))?;
    state
        .store
        .delete_project(project_id)
        .map_err(store_error)?;
    state.projects.remove(&project_id);
    for worktree_id in worktree_ids {
        state.worktrees.remove(&worktree_id);
    }
    Ok(())
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
    pty_tx: &mpsc::Sender<PtyEvent>,
) -> Result<Session, ProtocolError> {
    let cwd = session_parent_cwd(state, parent)?;
    let session = Session::new(name, parent, cwd.clone());
    let pty = ManagedPty::spawn(
        PtySpawnConfig::new(session.id, cwd).command(command),
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

fn list_draft_models(
    state: &Arc<Mutex<DaemonState>>,
    provider: DraftProvider,
) -> Result<Vec<String>, ProtocolError> {
    let config = draft_provider_config(state)?;
    drafts::list_models(&config, provider)
}

fn generate_commit_draft(
    state: &Arc<Mutex<DaemonState>>,
    worktree_id: WorktreeId,
    settings: Option<DraftGenerationSettings>,
) -> Result<CommitDraft, ProtocolError> {
    let worktree = refreshed_worktree_context(state, worktree_id)?.1;
    let provider = draft_provider_config(state)?.with_settings(settings);
    let repo = GitRepository::discover(&worktree.path).map_err(git_error)?;
    let summary = repo.status().map_err(git_error)?;
    let staged_paths = summary
        .entries
        .iter()
        .filter(|entry| entry.index != FileState::Unmodified)
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
    )
}

fn generate_pull_request_draft(
    state: &Arc<Mutex<DaemonState>>,
    worktree_id: WorktreeId,
    base: Option<String>,
    settings: Option<DraftGenerationSettings>,
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

    let commits = repo.commits_since(&base, 25).map_err(git_error)?;
    let commit_summaries = commits
        .iter()
        .filter_map(|commit| commit.summary.clone())
        .collect::<Vec<_>>();
    let changed_paths = repo.changed_paths_since(&base).map_err(git_error)?;
    let diff = repo.diff_since(&base).map_err(git_error)?;

    drafts::generate_pull_request_draft(
        &provider,
        PullRequestDraftInput {
            worktree_path: worktree.path,
            branch: worktree.branch,
            base,
            commits: commit_summaries,
            changed_paths,
            diff,
        },
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

fn spawn_pty_dispatcher(
    state: Arc<Mutex<DaemonState>>,
    rx: mpsc::Receiver<PtyEvent>,
    shutdown: Arc<AtomicBool>,
) {
    thread::Builder::new()
        .name("hitch-pty-dispatch".into())
        .spawn(move || {
            while let Ok(event) = rx.recv() {
                match event {
                    PtyEvent::Output { session_id, bytes } => {
                        persist_scrollback(&state, session_id);
                        let _ = broadcast_session_output(&state, session_id, &bytes);
                    }
                    PtyEvent::Exited {
                        session_id,
                        exit_code,
                    } => {
                        // During a graceful "Quit Hitch", kill_all_sessions kills the
                        // PTYs, which fires Exited here. Keep those sessions in the
                        // store so the next launch restores the layout as fresh
                        // terminals (ADR 0003); only forget sessions whose process
                        // exited while Hitch was running.
                        let shutting_down = shutdown.load(Ordering::SeqCst);
                        if let Ok(mut state) = state.lock() {
                            state.sessions.remove(&session_id);
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
                }
            }
        })
        .expect("failed to spawn PTY dispatch thread");
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
                let mut scrollback = daemon_session.restored_scrollback.clone();
                scrollback.extend(daemon_session.pty.scrollback());
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
    Ok(())
}

fn spawn_response_task<F>(
    name: &'static str,
    state: Arc<Mutex<DaemonState>>,
    client_id: u64,
    request_id: u64,
    job: F,
) -> Result<(), ProtocolError>
where
    F: FnOnce(&Arc<Mutex<DaemonState>>) -> Result<Response, ProtocolError> + Send + 'static,
{
    thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            let response = match job(&state) {
                Ok(response) => response,
                Err(error) => Response::Error { error },
            };
            let _ = send_response(&state, client_id, request_id, response);
        })
        .map(|_| ())
        .map_err(|err| internal(format!("failed to spawn {name}: {err}")))
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
    broadcast_with(state, |sink| write_output_to_sink(sink, session_id, bytes));
    Ok(())
}

fn broadcast_with<F>(state: &Arc<Mutex<DaemonState>>, mut send: F)
where
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
