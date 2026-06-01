//! Control-plane message types.
//!
//! These types are serialized as tagged JSON and intentionally contain no PTY
//! byte payloads. PTY bytes travel through `framing` helpers after a control
//! message announces the target session and byte count.

use std::path::PathBuf;

use hitch_core::{
    AgentState, JobId, Project, ProjectId, Session, SessionId, SessionParent, Worktree, WorktreeId,
};
use serde::{Deserialize, Serialize};

/// Current protocol version for daemon/socket compatibility checks.
///
/// v8 added the `Ping`/`Pong` heartbeat (ADR 0009) and the async **Job**
/// messages — `StartJob`/`CancelJob`, `JobStarted`, and the `JobProgress`/
/// `JobCompleted` events (ADR 0008). v9 extends `JobProgress` with optional
/// job-kind metadata so reconnecting clients can rebuild the live Job store. v10
/// adds the daemon pid to `Response::Hello` so a heartbeat-wedged daemon can be
/// force-restarted by pid. v11 bumps the protocol for the extended
/// `Response::PrStatus` wire shape. v12 moves PR status lookup to cancellable
/// Jobs via `JobRequest::PrStatus`. v13 adds the batched
/// `JobRequest::ProjectPrStatuses` so the sidebar can populate every worktree's
/// PR chip from one `gh pr list` per project instead of one lookup per visit.
pub const PROTOCOL_VERSION: u16 = 13;

/// Correlates a [`Request`] with a [`Response`] on the control plane.
pub type RequestId = u64;

/// Top-level JSON control-plane message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum ControlMessage {
    /// A command sent by `src-tauri`, `hitch-hook`, or a test client to the daemon.
    Request { id: RequestId, request: Request },
    /// The daemon's response to a previously received request.
    Response { id: RequestId, response: Response },
    /// Server-push notification delivered to subscribed clients.
    Event { event: Event },
}

impl ControlMessage {
    /// Build a request with the given id.
    pub fn request(id: RequestId, request: Request) -> Self {
        Self::Request { id, request }
    }

    /// Build a response with the given id.
    pub fn response(id: RequestId, response: Response) -> Self {
        Self::Response { id, response }
    }

    /// Build a server-push event.
    pub fn event(event: Event) -> Self {
        Self::Event { event }
    }
}

/// Long-running daemon operations that may run as an async **Job** (ADR 0008).
/// This is the exact allowlist accepted by [`Request::StartJob`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum JobRequest {
    /// Clone a remote into `destination` and add it as a project.
    CloneProject {
        remote_url: String,
        destination: PathBuf,
        name: Option<String>,
    },
    /// Create a managed worktree on a new or existing branch.
    CreateWorktree {
        project_id: ProjectId,
        branch: String,
        base: Option<String>,
        mode: WorktreeCreateMode,
    },
    /// List available Draft Generator models for a provider.
    ListDraftModels { provider: DraftProvider },
    /// Generate a commit draft from staged changes.
    GenerateCommitDraft {
        worktree_id: WorktreeId,
        settings: Option<DraftGenerationSettings>,
    },
    /// Generate a pull-request draft from branch context relative to `base`.
    GeneratePullRequestDraft {
        worktree_id: WorktreeId,
        base: Option<String>,
        settings: Option<DraftGenerationSettings>,
    },
    /// Push the current branch using the system `git` CLI.
    Push { worktree_id: WorktreeId },
    /// Pull the current branch from its upstream using the system `git` CLI.
    Pull { worktree_id: WorktreeId },
    /// Look up the GitHub PR (if any) for a worktree's current branch via `gh`.
    PrStatus { worktree_id: WorktreeId },
    /// Look up the PR for *every* worktree in a project in one `gh pr list`,
    /// mapped back to each worktree by branch. Lets the sidebar show PR chips
    /// for all worktrees without a per-worktree lookup.
    ProjectPrStatuses { project_id: ProjectId },
    /// Create a GitHub PR through `gh`.
    CreatePullRequest {
        worktree_id: WorktreeId,
        title: String,
        body: Option<String>,
        base: Option<String>,
        draft: bool,
    },
}

/// Client/hook commands accepted by the daemon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum Request {
    /// Compatibility handshake. The daemon replies with [`Response::Hello`].
    Hello {
        client_name: String,
        protocol_version: u16,
    },
    /// Explicitly stop the long-lived daemon (full quit).
    ShutdownDaemon,

    /// Return all projects known to the daemon.
    ListProjects,
    /// Add an existing local directory as a project.
    AddProject { root: PathBuf },
    /// Clone a remote into `destination` and add it as a project.
    CloneProject {
        remote_url: String,
        destination: PathBuf,
        name: Option<String>,
    },
    /// Forget a project and its Hitch-owned layout. Does not delete the project root.
    RemoveProject { project_id: ProjectId, force: bool },

    /// Return local and remote branches for a git-backed project.
    ListBranches { project_id: ProjectId },
    /// Return worktrees belonging to a git-backed project.
    ListWorktrees { project_id: ProjectId },
    /// Create a managed worktree on a new or existing branch.
    CreateWorktree {
        project_id: ProjectId,
        branch: String,
        base: Option<String>,
        mode: WorktreeCreateMode,
    },
    /// Remove a managed worktree, keeping the branch by default.
    RemoveWorktree {
        worktree_id: WorktreeId,
        delete_branch: bool,
        force: bool,
    },

    /// Return sessions, optionally scoped to a worktree or plain project root.
    ListSessions { parent: Option<SessionParent> },
    /// Spawn a new PTY session. `command == None` means default shell.
    /// `cols`/`rows` carry the client's initial terminal grid so the PTY spawns
    /// at the right size (no first-fit reflow flash); `0` means "use the daemon
    /// default" (mirrors [`ResizeSession`]'s fields).
    OpenSession {
        parent: SessionParent,
        name: String,
        command: Option<Vec<String>>,
        cols: u16,
        rows: u16,
    },
    /// Close a session. `kill_process` controls whether the PTY process is killed.
    CloseSession {
        session_id: SessionId,
        kill_process: bool,
    },
    /// Rename a session for display/layout persistence.
    RenameSession { session_id: SessionId, name: String },
    /// Announce that a raw PTY frame carrying input bytes follows this request.
    SendSessionInput {
        session_id: SessionId,
        byte_count: u32,
    },
    /// Resize the session's PTY.
    ResizeSession {
        session_id: SessionId,
        cols: u16,
        rows: u16,
    },
    /// Force the session's child to repaint. The daemon re-applies the PTY's
    /// current size and then sends SIGWINCH to the child's process group
    /// unconditionally — a same-size TIOCSWINSZ emits no SIGWINCH, so the
    /// explicit signal is what makes a full-screen app re-emit a correctly
    /// sized frame that overwrites garble.
    RepaintSession { session_id: SessionId },

    /// Read current git status for a worktree.
    GitStatus { worktree_id: WorktreeId },
    /// Look up the GitHub PR (if any) for a worktree's current branch via a Job.
    /// Kept as a compatibility request; dispatches through the async job path.
    PrStatus { worktree_id: WorktreeId },
    /// Look up the PR for every worktree in a project via a Job (batched).
    ProjectPrStatuses { project_id: ProjectId },
    /// Read a file-level diff for a path in a worktree.
    GitDiff {
        worktree_id: WorktreeId,
        path: PathBuf,
    },
    /// Stage whole files.
    StageFiles {
        worktree_id: WorktreeId,
        paths: Vec<PathBuf>,
    },
    /// Unstage whole files.
    UnstageFiles {
        worktree_id: WorktreeId,
        paths: Vec<PathBuf>,
    },
    /// Discard whole-file changes from the index and working tree.
    DiscardFiles {
        worktree_id: WorktreeId,
        paths: Vec<PathBuf>,
    },
    /// Commit staged files using the system `git` CLI.
    Commit {
        worktree_id: WorktreeId,
        subject: String,
        body: Option<String>,
    },
    /// List available Draft Generator models for a provider.
    ListDraftModels { provider: DraftProvider },
    /// Generate a commit draft from staged changes.
    GenerateCommitDraft {
        worktree_id: WorktreeId,
        settings: Option<DraftGenerationSettings>,
    },
    /// Generate a pull-request draft from branch context relative to `base`.
    GeneratePullRequestDraft {
        worktree_id: WorktreeId,
        base: Option<String>,
        settings: Option<DraftGenerationSettings>,
    },
    /// Push the current branch using the system `git` CLI.
    Push { worktree_id: WorktreeId },
    /// Pull the current branch from its upstream using the system `git` CLI.
    Pull { worktree_id: WorktreeId },
    /// Create a GitHub PR through `gh`.
    CreatePullRequest {
        worktree_id: WorktreeId,
        title: String,
        body: Option<String>,
        base: Option<String>,
        draft: bool,
    },

    /// Install/merge known-agent hooks in the target worktree.
    InstallAgentHooks { worktree_id: WorktreeId },
    /// Hook helper report: map a known agent hook event to Hitch Agent State.
    ReportAgentState {
        agent: KnownAgent,
        state: AgentState,
        session_id: Option<SessionId>,
        cwd: Option<PathBuf>,
        detail: Option<String>,
    },

    /// Liveness heartbeat (ADR 0009). The daemon replies with [`Response::Pong`].
    /// A responsive `Pong` is what makes the GUI's *running* status mean
    /// *responsive*, not merely socket-open.
    Ping,
    /// Run a long-running request off the per-client request loop as a **Job**
    /// (ADR 0008). The daemon replies immediately with [`Response::JobStarted`]
    /// and later broadcasts [`Event::JobCompleted`] carrying the wrapped
    /// request's final [`Response`]. Only [`JobRequest`] variants are accepted
    /// here; fast git reads stay synchronous.
    StartJob { request: JobRequest },
    /// Cancel a running Job, signalling its worker to kill any git/agent child.
    CancelJob { job_id: JobId },
}

impl From<JobRequest> for Request {
    fn from(request: JobRequest) -> Self {
        match request {
            JobRequest::CloneProject {
                remote_url,
                destination,
                name,
            } => Request::CloneProject {
                remote_url,
                destination,
                name,
            },
            JobRequest::CreateWorktree {
                project_id,
                branch,
                base,
                mode,
            } => Request::CreateWorktree {
                project_id,
                branch,
                base,
                mode,
            },
            JobRequest::ListDraftModels { provider } => Request::ListDraftModels { provider },
            JobRequest::GenerateCommitDraft {
                worktree_id,
                settings,
            } => Request::GenerateCommitDraft {
                worktree_id,
                settings,
            },
            JobRequest::GeneratePullRequestDraft {
                worktree_id,
                base,
                settings,
            } => Request::GeneratePullRequestDraft {
                worktree_id,
                base,
                settings,
            },
            JobRequest::Push { worktree_id } => Request::Push { worktree_id },
            JobRequest::Pull { worktree_id } => Request::Pull { worktree_id },
            JobRequest::PrStatus { worktree_id } => Request::PrStatus { worktree_id },
            JobRequest::ProjectPrStatuses { project_id } => {
                Request::ProjectPrStatuses { project_id }
            }
            JobRequest::CreatePullRequest {
                worktree_id,
                title,
                body,
                base,
                draft,
            } => Request::CreatePullRequest {
                worktree_id,
                title,
                body,
                base,
                draft,
            },
        }
    }
}

impl TryFrom<Request> for JobRequest {
    type Error = Request;

    fn try_from(request: Request) -> Result<Self, Self::Error> {
        match request {
            Request::CloneProject {
                remote_url,
                destination,
                name,
            } => Ok(JobRequest::CloneProject {
                remote_url,
                destination,
                name,
            }),
            Request::CreateWorktree {
                project_id,
                branch,
                base,
                mode,
            } => Ok(JobRequest::CreateWorktree {
                project_id,
                branch,
                base,
                mode,
            }),
            Request::ListDraftModels { provider } => Ok(JobRequest::ListDraftModels { provider }),
            Request::GenerateCommitDraft {
                worktree_id,
                settings,
            } => Ok(JobRequest::GenerateCommitDraft {
                worktree_id,
                settings,
            }),
            Request::GeneratePullRequestDraft {
                worktree_id,
                base,
                settings,
            } => Ok(JobRequest::GeneratePullRequestDraft {
                worktree_id,
                base,
                settings,
            }),
            Request::Push { worktree_id } => Ok(JobRequest::Push { worktree_id }),
            Request::Pull { worktree_id } => Ok(JobRequest::Pull { worktree_id }),
            Request::PrStatus { worktree_id } => Ok(JobRequest::PrStatus { worktree_id }),
            Request::ProjectPrStatuses { project_id } => {
                Ok(JobRequest::ProjectPrStatuses { project_id })
            }
            Request::CreatePullRequest {
                worktree_id,
                title,
                body,
                base,
                draft,
            } => Ok(JobRequest::CreatePullRequest {
                worktree_id,
                title,
                body,
                base,
                draft,
            }),
            other => Err(other),
        }
    }
}

/// How a worktree branch should be selected when creating a worktree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WorktreeCreateMode {
    /// Create a new branch from `base` (or repo default branch when absent).
    NewBranch,
    /// Check out an existing branch, if git permits it.
    ExistingBranch,
}

/// Known built-in agent integrations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum KnownAgent {
    /// Claude Code CLI.
    ClaudeCode,
    /// OpenAI Codex CLI.
    Codex,
}

/// Daemon replies to requests.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum Response {
    /// Successful compatibility handshake.
    Hello {
        protocol_version: u16,
        daemon_pid: u32,
    },
    /// Command succeeded and has no body.
    Ack,
    Projects {
        projects: Vec<Project>,
    },
    Branches {
        branches: Vec<BranchSummary>,
    },
    Worktrees {
        worktrees: Vec<Worktree>,
    },
    Sessions {
        sessions: Vec<Session>,
    },
    SessionOpened {
        session: Session,
    },
    GitStatus {
        status: GitStatus,
    },
    PrStatus {
        /// `None` when the branch has no PR (or `gh` could not determine one).
        pr: Option<PrInfo>,
    },
    /// PR per worktree for a whole project (reply to `ProjectPrStatuses`). A
    /// worktree with no matching PR carries `pr: None`.
    ProjectPrStatuses {
        statuses: Vec<WorktreePr>,
    },
    FileDiff {
        diff: FileDiff,
    },
    PullRequestCreated {
        url: String,
    },
    CommitDraft {
        draft: CommitDraft,
    },
    PullRequestDraft {
        draft: PullRequestDraft,
    },
    DraftModels {
        provider: DraftProvider,
        models: Vec<String>,
    },
    /// Heartbeat reply to [`Request::Ping`] (ADR 0009).
    Pong,
    /// A Job was accepted and is now running off the request loop (ADR 0008).
    /// The wrapped request's real [`Response`] arrives later inside
    /// [`Event::JobCompleted`].
    JobStarted {
        job_id: JobId,
    },
    /// Command failed. Kept in-band so clients can correlate by request id.
    Error {
        error: ProtocolError,
    },
}

/// Lifecycle of an async **Job** (`CONTEXT.md`). A Job is *queued*, then
/// *running*, and finishes in exactly one terminal state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum JobStatus {
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

/// Server-push notifications delivered by the daemon to clients.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum Event {
    SessionOpened {
        session: Session,
    },
    SessionClosed {
        session_id: SessionId,
        exit_code: Option<i32>,
    },
    /// A raw PTY data frame of `byte_count` bytes follows this event.
    SessionOutput {
        session_id: SessionId,
        byte_count: u32,
    },
    AgentState {
        session_id: Option<SessionId>,
        worktree_id: Option<WorktreeId>,
        agent: KnownAgent,
        state: AgentState,
        detail: Option<String>,
    },
    WorktreeDirty {
        worktree_id: WorktreeId,
        dirty: bool,
    },
    WorktreeUpdated {
        worktree: Worktree,
    },
    ProjectUpdated {
        project: Project,
    },
    /// A project (and its Hitch-owned worktrees/sessions) was removed; peers
    /// should drop it from their view.
    ProjectRemoved {
        project_id: ProjectId,
    },
    /// The session's foreground process changed — the live command the user is
    /// interacting with in the PTY (e.g. a tool launched inside the shell),
    /// not the spawn command. `None` when it can't be resolved.
    SessionCommand {
        session_id: SessionId,
        command: Option<String>,
    },
    /// A **Job** changed lifecycle state or emitted a progress note (ADR 0008).
    /// Broadcast to every attached GUI; clients track it by `job_id`.
    JobProgress {
        job_id: JobId,
        status: JobStatus,
        /// Optional human-readable progress note (e.g. "Pushing…").
        message: Option<String>,
        /// Stable UI-facing job kind (e.g. `push`, `pr-draft`) when known.
        kind: Option<String>,
    },
    /// A **Job** finished. The wrapped request's final [`Response`] rides inside
    /// (`Response::Ack` / `PullRequestCreated` / `CommitDraft` / …, or
    /// `Response::Error` on failure). The GUI resolves the awaiting caller from
    /// this. Boxed because [`Response`] is large relative to the other variants.
    JobCompleted {
        job_id: JobId,
        response: Box<Response>,
    },
}

/// A branch name with remote flag for branch-picker UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BranchSummary {
    pub name: String,
    pub is_remote: bool,
}

/// Git status summary used by the focused common-flow UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitStatus {
    pub worktree_id: WorktreeId,
    pub branch: String,
    pub dirty: bool,
    pub ahead: u32,
    pub behind: u32,
    #[serde(default)]
    pub additions: u32,
    #[serde(default)]
    pub deletions: u32,
    pub files: Vec<ChangedFile>,
}

/// An existing GitHub pull request for a worktree's current branch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrInfo {
    pub number: u64,
    pub url: String,
    /// `gh`'s PR state, e.g. `OPEN`, `CLOSED`, `MERGED`.
    pub state: String,
    pub draft: bool,
}

/// A worktree paired with the PR (if any) for its branch — one entry per
/// worktree in a [`Response::ProjectPrStatuses`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreePr {
    pub worktree_id: WorktreeId,
    pub pr: Option<PrInfo>,
}

/// One changed path in a worktree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangedFile {
    pub path: PathBuf,
    pub status: FileStatus,
    pub staged: bool,
}

/// Coarse file status values needed by the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FileStatus {
    Added,
    Modified,
    Deleted,
    Renamed,
    Copied,
    Untracked,
    Conflicted,
}

/// File-level diff response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileDiff {
    pub worktree_id: WorktreeId,
    pub path: PathBuf,
    pub diff: String,
}

/// Client-selected Draft Generator provider settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftGenerationSettings {
    pub provider: DraftProvider,
    pub model: Option<String>,
}

/// Headless provider used for Draft Generator runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DraftProvider {
    Stub,
    Claude,
    Codex,
}

/// Generated commit text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitDraft {
    pub subject: String,
    pub body: String,
}

/// Generated pull-request text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestDraft {
    pub title: String,
    pub body: String,
}

/// Structured in-band protocol/application error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtocolError {
    pub code: ErrorCode,
    pub message: String,
    pub retryable: bool,
}

impl ProtocolError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            retryable: false,
        }
    }

    pub fn retryable(mut self, retryable: bool) -> Self {
        self.retryable = retryable;
        self
    }
}

/// Stable error categories for clients.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ErrorCode {
    InvalidRequest,
    UnsupportedProtocol,
    NotFound,
    AlreadyExists,
    DirtyWorktree,
    LiveSessions,
    GitFailed,
    PtyFailed,
    StoreFailed,
    AgentHookFailed,
    Unauthorized,
    Unavailable,
    Internal,
}

#[cfg(test)]
mod tests {
    use super::*;
    use hitch_core::{ProjectKind, Worktree};

    #[test]
    fn control_messages_round_trip() {
        for message in sample_control_messages() {
            let json = serde_json::to_string(&message).unwrap();
            let back: ControlMessage = serde_json::from_str(&json).unwrap();
            assert_eq!(message, back, "failed to round-trip {json}");
        }
    }

    #[test]
    fn request_variants_round_trip() {
        for request in sample_requests() {
            let json = serde_json::to_string(&request).unwrap();
            let back: Request = serde_json::from_str(&json).unwrap();
            assert_eq!(request, back, "failed to round-trip {json}");
        }
    }

    #[test]
    fn job_request_variants_round_trip() {
        let (project_id, worktree_id, _) = ids();
        for request in sample_job_requests(project_id, worktree_id) {
            let json = serde_json::to_string(&request).unwrap();
            let back: JobRequest = serde_json::from_str(&json).unwrap();
            assert_eq!(request, back, "failed to round-trip {json}");
        }
    }

    #[test]
    fn response_variants_round_trip() {
        for response in sample_responses() {
            let json = serde_json::to_string(&response).unwrap();
            let back: Response = serde_json::from_str(&json).unwrap();
            assert_eq!(response, back, "failed to round-trip {json}");
        }
    }

    #[test]
    fn event_variants_round_trip() {
        for event in sample_events() {
            let json = serde_json::to_string(&event).unwrap();
            let back: Event = serde_json::from_str(&json).unwrap();
            assert_eq!(event, back, "failed to round-trip {json}");
        }
    }

    #[test]
    fn git_status_deserializes_without_line_stats_for_rolling_upgrades() {
        let (_, worktree_id, _) = ids();
        let json = format!(
            r#"{{"type":"git-status","status":{{"worktree_id":"{worktree_id}","branch":"main","dirty":true,"ahead":0,"behind":0,"files":[]}}}}"#
        );
        let back: Response = serde_json::from_str(&json).unwrap();

        let Response::GitStatus { status } = back else {
            panic!("expected git-status response");
        };
        assert_eq!(status.additions, 0);
        assert_eq!(status.deletions, 0);
    }

    #[test]
    fn session_command_event_serializes_as_contract() {
        let (_, _, session_id) = ids();
        let event = Event::SessionCommand {
            session_id,
            command: Some("claude".into()),
        };
        let value: serde_json::Value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["type"], "session-command");
        assert_eq!(value["session_id"], session_id.to_string());
        assert_eq!(value["command"], "claude");

        let back: Event = serde_json::from_value(value).unwrap();
        assert_eq!(event, back);

        let cleared = Event::SessionCommand {
            session_id,
            command: None,
        };
        let value: serde_json::Value = serde_json::to_value(&cleared).unwrap();
        assert!(value["command"].is_null());
    }

    #[test]
    fn open_session_carries_initial_size_as_contract() {
        let (_, worktree_id, _) = ids();
        let request = Request::OpenSession {
            parent: SessionParent::Worktree(worktree_id),
            name: "shell".into(),
            command: Some(vec!["zsh".into()]),
            cols: 132,
            rows: 50,
        };
        let value: serde_json::Value = serde_json::to_value(&request).unwrap();
        assert_eq!(value["type"], "open-session");
        assert_eq!(value["cols"], 132);
        assert_eq!(value["rows"], 50);

        let back: Request = serde_json::from_value(value).unwrap();
        assert_eq!(request, back);
    }

    #[test]
    fn catalog_mentions_core_message_families() {
        assert!(crate::MESSAGE_CATALOG.contains("Request:"));
        assert!(crate::MESSAGE_CATALOG.contains("Response:"));
        assert!(crate::MESSAGE_CATALOG.contains("Event:"));
        assert!(crate::MESSAGE_CATALOG.contains("session-output"));
        assert!(crate::MESSAGE_CATALOG.contains("worktree-dirty"));
        // v8 heartbeat + Job families (ADR 0008/0009).
        assert!(crate::MESSAGE_CATALOG.contains("ping"));
        assert!(crate::MESSAGE_CATALOG.contains("start-job"));
        assert!(crate::MESSAGE_CATALOG.contains("job-completed"));
    }

    #[test]
    fn ping_pong_serialize_as_bare_tagged_variants() {
        // The heartbeat carries no payload: a bare `{"type":"ping"}` /
        // `{"type":"pong"}`. This is the contract the GUI's heartbeat thread
        // round-trips on every interval (ADR 0009).
        let ping: serde_json::Value = serde_json::to_value(Request::Ping).unwrap();
        assert_eq!(ping["type"], "ping");
        let pong: serde_json::Value = serde_json::to_value(Response::Pong).unwrap();
        assert_eq!(pong["type"], "pong");
    }

    #[test]
    fn job_messages_wrap_inner_request_and_response_as_contract() {
        let (_, worktree_id, _) = ids();
        let job_id = JobId::new();

        // StartJob wraps only the supported JobRequest allowlist.
        let start = Request::StartJob {
            request: JobRequest::Push { worktree_id },
        };
        let value: serde_json::Value = serde_json::to_value(&start).unwrap();
        assert_eq!(value["type"], "start-job");
        assert_eq!(value["request"]["type"], "push");
        let back: Request = serde_json::from_value(value).unwrap();
        assert_eq!(start, back);

        let unsupported = serde_json::json!({
            "type": "start-job",
            "request": {
                "type": "git-status",
                "worktree_id": worktree_id,
            },
        });
        serde_json::from_value::<Request>(unsupported).unwrap_err();

        // JobCompleted carries the wrapped request's real Response inside.
        let completed = Event::JobCompleted {
            job_id,
            response: Box::new(Response::PullRequestCreated {
                url: "https://example/pull/1".into(),
            }),
        };
        let value: serde_json::Value = serde_json::to_value(&completed).unwrap();
        assert_eq!(value["type"], "job-completed");
        assert_eq!(value["response"]["type"], "pull-request-created");
        let back: Event = serde_json::from_value(value).unwrap();
        assert_eq!(completed, back);

        // JobStatus stringifies kebab-case so the frontend store can match it,
        // and reconnect snapshots can carry the original UI job kind.
        let progress: serde_json::Value = serde_json::to_value(Event::JobProgress {
            job_id,
            status: JobStatus::Cancelled,
            message: None,
            kind: Some("push".into()),
        })
        .unwrap();
        assert_eq!(progress["status"], "cancelled");
        assert_eq!(progress["kind"], "push");
    }

    fn ids() -> (ProjectId, WorktreeId, SessionId) {
        (ProjectId::new(), WorktreeId::new(), SessionId::new())
    }

    fn sample_project(project_id: ProjectId) -> Project {
        Project {
            id: project_id,
            name: "hitch".into(),
            root: "/Users/me/Code/hitch".into(),
            kind: ProjectKind::GitBacked,
        }
    }

    fn sample_worktree(project_id: ProjectId, worktree_id: WorktreeId) -> Worktree {
        Worktree {
            id: worktree_id,
            project_id,
            path: "/Users/me/.hitch/worktrees/hitch/feat-proto".into(),
            branch: "feat/proto".into(),
            is_main: false,
            is_hitch_managed: true,
        }
    }

    fn sample_job_requests(project_id: ProjectId, worktree_id: WorktreeId) -> Vec<JobRequest> {
        vec![
            JobRequest::CloneProject {
                remote_url: "https://example.com/hitch.git".into(),
                destination: "/Users/me/Code/hitch".into(),
                name: Some("hitch".into()),
            },
            JobRequest::CreateWorktree {
                project_id,
                branch: "feat/proto".into(),
                base: Some("main".into()),
                mode: WorktreeCreateMode::NewBranch,
            },
            JobRequest::ListDraftModels {
                provider: DraftProvider::Codex,
            },
            JobRequest::GenerateCommitDraft {
                worktree_id,
                settings: Some(DraftGenerationSettings {
                    provider: DraftProvider::Claude,
                    model: Some("sonnet".into()),
                }),
            },
            JobRequest::GeneratePullRequestDraft {
                worktree_id,
                base: Some("main".into()),
                settings: Some(DraftGenerationSettings {
                    provider: DraftProvider::Codex,
                    model: Some("gpt-5-codex".into()),
                }),
            },
            JobRequest::Push { worktree_id },
            JobRequest::Pull { worktree_id },
            JobRequest::PrStatus { worktree_id },
            JobRequest::CreatePullRequest {
                worktree_id,
                title: "Add proto".into(),
                body: Some("Body".into()),
                base: Some("main".into()),
                draft: true,
            },
        ]
    }

    fn sample_session(worktree_id: WorktreeId, session_id: SessionId) -> Session {
        Session {
            id: session_id,
            name: "shell".into(),
            parent: SessionParent::Worktree(worktree_id),
            cwd: "/Users/me/.hitch/worktrees/hitch/feat-proto".into(),
        }
    }

    fn sample_status(worktree_id: WorktreeId) -> GitStatus {
        GitStatus {
            worktree_id,
            branch: "feat/proto".into(),
            dirty: true,
            ahead: 1,
            behind: 0,
            additions: 12,
            deletions: 3,
            files: vec![ChangedFile {
                path: "src/lib.rs".into(),
                status: FileStatus::Modified,
                staged: false,
            }],
        }
    }

    fn sample_diff(worktree_id: WorktreeId) -> FileDiff {
        FileDiff {
            worktree_id,
            path: "src/lib.rs".into(),
            diff: "diff --git a/src/lib.rs b/src/lib.rs".into(),
        }
    }

    fn sample_requests() -> Vec<Request> {
        let (project_id, worktree_id, session_id) = ids();
        vec![
            Request::Hello {
                client_name: "src-tauri".into(),
                protocol_version: PROTOCOL_VERSION,
            },
            Request::ShutdownDaemon,
            Request::ListProjects,
            Request::AddProject {
                root: "/repo".into(),
            },
            Request::CloneProject {
                remote_url: "https://github.com/example/hitch.git".into(),
                destination: "/tmp/hitch".into(),
                name: Some("hitch".into()),
            },
            Request::RemoveProject {
                project_id,
                force: true,
            },
            Request::ListWorktrees { project_id },
            Request::CreateWorktree {
                project_id,
                branch: "feat/proto".into(),
                base: Some("main".into()),
                mode: WorktreeCreateMode::NewBranch,
            },
            Request::RemoveWorktree {
                worktree_id,
                delete_branch: false,
                force: true,
            },
            Request::ListSessions {
                parent: Some(SessionParent::Worktree(worktree_id)),
            },
            Request::OpenSession {
                parent: SessionParent::Worktree(worktree_id),
                name: "shell".into(),
                command: Some(vec!["zsh".into(), "-l".into()]),
                cols: 120,
                rows: 40,
            },
            Request::CloseSession {
                session_id,
                kill_process: true,
            },
            Request::RenameSession {
                session_id,
                name: "agent".into(),
            },
            Request::SendSessionInput {
                session_id,
                byte_count: 4,
            },
            Request::ResizeSession {
                session_id,
                cols: 120,
                rows: 40,
            },
            Request::RepaintSession { session_id },
            Request::GitStatus { worktree_id },
            Request::PrStatus { worktree_id },
            Request::GitDiff {
                worktree_id,
                path: "src/lib.rs".into(),
            },
            Request::StageFiles {
                worktree_id,
                paths: vec!["src/lib.rs".into()],
            },
            Request::UnstageFiles {
                worktree_id,
                paths: vec!["src/lib.rs".into()],
            },
            Request::DiscardFiles {
                worktree_id,
                paths: vec!["src/lib.rs".into()],
            },
            Request::Commit {
                worktree_id,
                subject: "feat: add proto".into(),
                body: Some("Body".into()),
            },
            Request::ListDraftModels {
                provider: DraftProvider::Codex,
            },
            Request::GenerateCommitDraft {
                worktree_id,
                settings: Some(DraftGenerationSettings {
                    provider: DraftProvider::Claude,
                    model: Some("sonnet".into()),
                }),
            },
            Request::GeneratePullRequestDraft {
                worktree_id,
                base: Some("main".into()),
                settings: Some(DraftGenerationSettings {
                    provider: DraftProvider::Codex,
                    model: Some("gpt-5-codex".into()),
                }),
            },
            Request::Push { worktree_id },
            Request::Pull { worktree_id },
            Request::CreatePullRequest {
                worktree_id,
                title: "Add proto".into(),
                body: Some("Body".into()),
                base: Some("main".into()),
                draft: true,
            },
            Request::InstallAgentHooks { worktree_id },
            Request::ReportAgentState {
                agent: KnownAgent::ClaudeCode,
                state: AgentState::NeedsApproval,
                session_id: Some(session_id),
                cwd: Some("/repo".into()),
                detail: Some("permission prompt".into()),
            },
            Request::Ping,
            Request::StartJob {
                request: JobRequest::Push { worktree_id },
            },
            Request::CancelJob {
                job_id: JobId::new(),
            },
        ]
    }

    fn sample_responses() -> Vec<Response> {
        let (project_id, worktree_id, session_id) = ids();
        let project = sample_project(project_id);
        let worktree = sample_worktree(project_id, worktree_id);
        let session = sample_session(worktree_id, session_id);
        vec![
            Response::Hello {
                protocol_version: PROTOCOL_VERSION,
                daemon_pid: 42,
            },
            Response::Ack,
            Response::Projects {
                projects: vec![project],
            },
            Response::Worktrees {
                worktrees: vec![worktree],
            },
            Response::Sessions {
                sessions: vec![session.clone()],
            },
            Response::SessionOpened { session },
            Response::GitStatus {
                status: sample_status(worktree_id),
            },
            Response::PrStatus {
                pr: Some(PrInfo {
                    number: 1,
                    url: "https://github.com/example/hitch/pull/1".into(),
                    state: "OPEN".into(),
                    draft: false,
                }),
            },
            Response::FileDiff {
                diff: sample_diff(worktree_id),
            },
            Response::PullRequestCreated {
                url: "https://github.com/example/hitch/pull/1".into(),
            },
            Response::CommitDraft {
                draft: CommitDraft {
                    subject: "chore: update src".into(),
                    body: "- Update src/lib.rs".into(),
                },
            },
            Response::PullRequestDraft {
                draft: PullRequestDraft {
                    title: "Add proto".into(),
                    body: "## Summary\n\n- Update src/lib.rs\n\n## Testing\n\n- [ ] Not run".into(),
                },
            },
            Response::DraftModels {
                provider: DraftProvider::Codex,
                models: vec!["gpt-5-codex".into(), "gpt-5".into()],
            },
            Response::Pong,
            Response::JobStarted {
                job_id: JobId::new(),
            },
            Response::Error {
                error: ProtocolError::new(ErrorCode::Unavailable, "daemon busy").retryable(true),
            },
        ]
    }

    fn sample_events() -> Vec<Event> {
        let (project_id, worktree_id, session_id) = ids();
        let project = sample_project(project_id);
        let worktree = sample_worktree(project_id, worktree_id);
        let session = sample_session(worktree_id, session_id);
        vec![
            Event::SessionOpened { session },
            Event::SessionClosed {
                session_id,
                exit_code: Some(0),
            },
            Event::SessionOutput {
                session_id,
                byte_count: 12,
            },
            Event::AgentState {
                session_id: Some(session_id),
                worktree_id: Some(worktree_id),
                agent: KnownAgent::Codex,
                state: AgentState::Running,
                detail: None,
            },
            Event::WorktreeDirty {
                worktree_id,
                dirty: true,
            },
            Event::WorktreeUpdated { worktree },
            Event::ProjectUpdated { project },
            Event::ProjectRemoved { project_id },
            Event::SessionCommand {
                session_id,
                command: Some("claude".into()),
            },
            Event::SessionCommand {
                session_id,
                command: None,
            },
            Event::JobProgress {
                job_id: JobId::new(),
                status: JobStatus::Running,
                message: Some("Pushing…".into()),
                kind: Some("push".into()),
            },
            Event::JobCompleted {
                job_id: JobId::new(),
                response: Box::new(Response::Ack),
            },
        ]
    }

    fn sample_control_messages() -> Vec<ControlMessage> {
        vec![
            ControlMessage::request(1, sample_requests().remove(0)),
            ControlMessage::response(1, sample_responses().remove(0)),
            ControlMessage::event(sample_events().remove(0)),
        ]
    }
}
