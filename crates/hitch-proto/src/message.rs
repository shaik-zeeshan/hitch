//! Control-plane message types.
//!
//! These types are serialized as tagged JSON and intentionally contain no PTY
//! byte payloads. PTY bytes travel through `framing` helpers after a control
//! message announces the target session and byte count.

use std::path::PathBuf;

use hitch_core::{
    AgentState, Project, ProjectId, Session, SessionId, SessionParent, Worktree, WorktreeId,
};
use serde::{Deserialize, Serialize};

/// Current protocol version for daemon/socket compatibility checks.
pub const PROTOCOL_VERSION: u16 = 2;

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
    OpenSession {
        parent: SessionParent,
        name: String,
        command: Option<Vec<String>>,
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

    /// Read current git status for a worktree.
    GitStatus { worktree_id: WorktreeId },
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
        message: String,
    },
    /// Push the current branch using the system `git` CLI.
    Push { worktree_id: WorktreeId },
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
    },
    /// Command succeeded and has no body.
    Ack,
    Projects {
        projects: Vec<Project>,
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
    FileDiff {
        diff: FileDiff,
    },
    PullRequestCreated {
        url: String,
    },
    /// Command failed. Kept in-band so clients can correlate by request id.
    Error {
        error: ProtocolError,
    },
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
    /// The session's foreground process changed — the live command the user is
    /// interacting with in the PTY (e.g. a tool launched inside the shell),
    /// not the spawn command. `None` when it can't be resolved.
    SessionCommand {
        session_id: SessionId,
        command: Option<String>,
    },
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
    fn catalog_mentions_core_message_families() {
        assert!(crate::MESSAGE_CATALOG.contains("Request:"));
        assert!(crate::MESSAGE_CATALOG.contains("Response:"));
        assert!(crate::MESSAGE_CATALOG.contains("Event:"));
        assert!(crate::MESSAGE_CATALOG.contains("session-output"));
        assert!(crate::MESSAGE_CATALOG.contains("worktree-dirty"));
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
        }
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
            Request::GitStatus { worktree_id },
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
                message: "feat: add proto".into(),
            },
            Request::Push { worktree_id },
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
            Response::FileDiff {
                diff: sample_diff(worktree_id),
            },
            Response::PullRequestCreated {
                url: "https://github.com/example/hitch/pull/1".into(),
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
            Event::SessionCommand {
                session_id,
                command: Some("claude".into()),
            },
            Event::SessionCommand {
                session_id,
                command: None,
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
