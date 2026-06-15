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
/// v14 makes reported/broadcast agent state nullable (`null` clears daemon-owned
/// state) and includes current agent metadata on session-open replay payloads.
/// v15 adds `Request::RepaintSession` so the GUI can ask the daemon to force a
/// child-process repaint after activation or resize. v16 adds fetch as an
/// explicit Job so remote refs can be refreshed without blocking the request loop.
/// v17 adds `Event::WorktreeRemoved` so peers can drop daemon-owned removed
/// worktrees without waiting for a full tree refresh. v18 adds Draft Generator
/// CLI path overrides and passes draft settings into model-discovery Jobs. v19
/// adds `Request::AnnounceAgent`, the hook helper's identity-only announce (ADR
/// 0011 amendment 2026-06-05) — it carries *which* agent with no state field, so
/// the Session mark can render before the first prompt without being confused
/// with a `state: None` clear. v20 adds `Event::OutputActive`, the daemon's
/// edge-triggered per-session output-activity transition (ADR 0011 amendment
/// 2026-06-05) — the gate behind the `WORKING` display word
/// (`running` AND output-active) — and `output_active` on the `SessionOpened`
/// replay so a newly attached client starts with the correct gate value instead
/// of assuming. v21 adds optional `agent_run_id` to agent hook reports/announces
/// so the daemon can drop stale lifecycle hooks from a previous agent process
/// after a newer `SessionStart`. v22 extends the diff wire contract:
/// `Request::GitDiff` gains `staged`/`ignore_whitespace`/`context_lines` so the
/// client can request the correct diff side and rendering, and `ChangedFile`
/// gains `additions`/`deletions` line counts — an old daemon at v21 would
/// otherwise serve wrong-side diffs and zeroed counts. v23 adds the History
/// reads: `Request::GitLog`/`Response::CommitLog` (paginated enriched commit
/// log) and `Request::CommitDiff`/`Response::CommitDiff` (one commit's
/// first-parent per-file diff), and adds `head_commit_id` to `GitStatus` so the
/// log can refetch off the existing status backbone — an old daemon at v22 has
/// neither request nor the head id. v24 adds optional `commit_instructions`/
/// `pr_instructions` to `DraftGenerationSettings` — the Composer's Draft
/// Instructions, appended to the built-in draft prompts as an extra block
/// (ADR 0007 amendment 2026-06-07); an old daemon at v23 ignores them, so a
/// client's instructions would silently not reach the prompt. v24 also adds the
/// daemon-owned composite Job chains (ADR 0013 amendment 2026-06-07): the
/// `JobRequest::CommitAndPush`/`JobRequest::CreatePr` kinds, the per-step
/// `Event::CompositeJobProgress` broadcast, the `Response::CommitAndPushed`
/// completion payload (and `Response::CompositeJobFailed` for a mid-chain
/// failure carrying prior steps' results), and the `Request::ActiveJobs` /
/// `Response::ActiveJobs` query a re-attaching GUI uses to restore button state
/// from a worktree's in-flight chains. An old daemon at v23 has none of these,
/// so a client's composite chain would never run. v25 adds
/// `Request::ListDirectory`/`Response::DirectoryListing` (the remote directory
/// browser backing "Add Project inside an SSH Host scope", ADR 0014) — a fast
/// synchronous filesystem read, not a Job, that lets the GUI navigate a remote
/// daemon user's readable directories (folders-first, hidden-folder toggle) and
/// type an absolute path before sending the existing AddProject/CloneProject to
/// that remote daemon. An old daemon at v24 has neither request nor response.
/// v26 adds the file-drop **upload** protocol (issue #31, ADR 0014): dropping
/// local files onto a remote Session streams them over this same stream to the
/// remote daemon, which writes them under `<data-dir>/uploads/<session-id>/` and
/// returns the actual remote paths for the GUI to insert. The chunked exchange is
/// `Request::BeginUpload` → `Response::UploadStarted`, repeated
/// `Request::UploadChunk` (+ a length-prefixed PTY-style raw frame) → ack, then
/// `Request::FinishUpload` → `Response::UploadFinished { remote_path }`, with
/// `Request::AbortUpload` deleting a partial. Chunks are bounded (256 KiB) so
/// they interleave with interactive PTY traffic. v26 also adds `os_family` to
/// `Response::Hello` so the GUI quotes inserted remote paths for the remote
/// platform (POSIX vs Windows). An old daemon at v25 has neither the upload
/// messages nor the platform field.
/// v27 adds `exe_path` (the daemon's startup `current_exe()` absolute path) to
/// `Response::Hello` so the client can cache it and re-invoke the daemon binary
/// directly on reconnect (approach C, ADR 0014 amendment), shell-free and
/// PATH-free — identical resolution across OS without relying on the
/// non-interactive `ssh host cmd` PATH. An old daemon at v26 omits it, so the
/// field decodes to `None` and the client falls back to its candidate-path
/// probe (the known Unix self-install location, then bare `hitch`).
/// v28 adds the `ConnEnv` control message the SSH proxy emits to declare its
/// forwarded `SSH_AUTH_SOCK` (ADR 0014, agent forwarding). Because the proxy
/// sends it as a connection prelude — ahead of the GUI's Hello, and a frame an
/// older daemon can't parse — a persistent remote daemon MUST be restarted onto
/// a v28 binary after upgrading; an unrestarted v27 daemon chokes on the unknown
/// frame before it answers the Hello. Bumping the version makes the GUI reject
/// any still-reachable older daemon as a clean protocol mismatch.
/// v29 adds the **ssh-agent relay** (ADR 0014 amendment): the GUI declares
/// [`ControlMessage::SshAgentRelay`] on a remote connection it can sign for, and
/// the daemon then tunnels the ssh-agent wire protocol over
/// [`ControlMessage::SshAgentOpen`]/[`SshAgentData`]/[`SshAgentClose`] so a
/// detached remote daemon signs `push/pull/fetch/clone` with the user's *local*
/// agent — reaching where OS `ForwardAgent` (the v28 `ConnEnv` path) cannot. The
/// same restart lesson applies: a persistent remote daemon MUST be restarted onto
/// a v29 binary or it chokes on the unknown frames before answering Hello.
/// v30 adds [`ControlMessage::ClientActive`] (ADR 0014 amendment): a GUI →
/// daemon focus-gain ping the GUI sends when it becomes the foreground app, so
/// the daemon can refresh its "driving client" — the most-recently-active
/// relay-capable connection it routes ssh-agent signing to (presence routing for
/// terminal & Agent-run git). Parameterless; the client_id is known from the
/// connection. An old daemon at v29 has no such message, so a still-reachable
/// older daemon is rejected as a clean protocol mismatch.
pub const PROTOCOL_VERSION: u16 = 30;

/// Maximum bytes carried by one [`Request::UploadChunk`] frame (256 KiB). The
/// upload stream is shared with interactive PTY traffic, so chunks are bounded
/// well under [`crate::framing::MAX_PTY_FRAME_LEN`]: each chunk is one
/// request/response turn, giving PTY frames a natural slot between chunks instead
/// of letting one giant frame starve a live session.
pub const UPLOAD_CHUNK_BYTES: usize = 256 * 1024;

/// Defensive upper bound on the *decoded* byte length of a single
/// [`ControlMessage::SshAgentData`] relay frame (256 KiB), matching OpenSSH's own
/// ssh-agent message bound. ssh-agent sign requests/replies are sub-KiB, so this
/// is pure abuse-resistance: a peer that base64-encodes a larger frame is
/// rejected by [`decode_ssh_agent_data`] before the bytes are ever forwarded to a
/// socket. Like [`UPLOAD_CHUNK_BYTES`] it keeps one relay frame from starving the
/// PTY traffic that shares the same control channel.
pub const SSH_AGENT_RELAY_MAX_FRAME: usize = 256 * 1024;

/// Identifies one in-flight file upload, minted by the daemon in
/// [`Response::UploadStarted`] and echoed by the client on every
/// [`Request::UploadChunk`]/[`FinishUpload`]/[`AbortUpload`]. A plain string so
/// the daemon picks the representation (it uses a uuid).
pub type UploadId = String;

/// The daemon host's OS family, carried on [`Response::Hello`] so the GUI can
/// quote inserted remote upload paths for the right shell (issue #31). Only the
/// POSIX/Windows split matters for path quoting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OsFamily {
    Unix,
    Windows,
}

impl OsFamily {
    /// The family of the daemon currently running. Resolved at compile time from
    /// the target so the daemon reports its own platform, not the GUI's.
    pub fn current() -> Self {
        #[cfg(windows)]
        {
            OsFamily::Windows
        }
        #[cfg(not(windows))]
        {
            OsFamily::Unix
        }
    }
}

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
    /// Connection-environment prelude emitted by the SSH **proxy** (ADR 0014)
    /// before it begins its verbatim GUI↔daemon bridge. It carries the proxy
    /// process's forwarded `SSH_AUTH_SOCK` path so the long-lived remote daemon
    /// can sign git pushes via the *local* user's forwarded ssh-agent instead of
    /// prompting on the remote. Serialized as `{"kind":"conn-env",...}`. Backward
    /// compatible: the GUI never emits it, and an older/local (non-proxy) peer
    /// never sees it — a local daemon's git keeps inheriting env exactly as before.
    ConnEnv { ssh_auth_sock: String },
    /// GUI → daemon **ssh-agent relay** capability declaration (v29, ADR 0014
    /// amendment). Sent as a connection prelude on a *remote* connection the GUI
    /// can sign for (local agent reachable + per-host toggle on), it tells the
    /// daemon "you may host a per-connection ssh-agent socket for my git ops and
    /// relay its bytes back to me." The GUI never sends it on a local connection
    /// (a local daemon inherits the real agent env). Serialized as
    /// `{"kind":"ssh-agent-relay"}`.
    SshAgentRelay,
    /// Daemon → GUI: a git child connected to the daemon-hosted ssh-agent socket;
    /// `channel` identifies this connection so concurrent signings (a push and a
    /// fetch at once) stay distinct. The GUI opens a matching bridge to its local
    /// agent. Serialized as `{"kind":"ssh-agent-open",...}`.
    SshAgentOpen { channel: u64 },
    /// Both ways: one chunk of raw ssh-agent wire bytes for `channel`, base64 in
    /// `data` because the bytes are binary and the control channel is
    /// newline-framed JSON the proxy bridges verbatim. Decode/size-check with
    /// [`decode_ssh_agent_data`] (cap [`SSH_AGENT_RELAY_MAX_FRAME`]); build with
    /// [`ControlMessage::ssh_agent_data`]. Serialized as `{"kind":"ssh-agent-data",...}`.
    SshAgentData { channel: u64, data: String },
    /// Both ways: `channel` reached EOF or was torn down — the git-side socket
    /// closed (daemon → GUI) or the local agent connection closed (GUI → daemon).
    /// Serialized as `{"kind":"ssh-agent-close",...}`.
    SshAgentClose { channel: u64 },
    /// GUI → daemon focus-gain ping: this GUI is now the foreground app and should
    /// become the driving client for ssh-agent relay routing. Parameterless — the
    /// client_id is known from the connection. (v30, ADR 0014 amendment)
    ClientActive,
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

    /// Build an [`SshAgentData`](ControlMessage::SshAgentData) frame, base64
    /// encoding the raw ssh-agent wire `bytes` so they survive the newline-framed
    /// JSON control channel. The decode side is [`decode_ssh_agent_data`].
    pub fn ssh_agent_data(channel: u64, bytes: &[u8]) -> Self {
        Self::SshAgentData {
            channel,
            data: encode_ssh_agent_data(bytes),
        }
    }
}

/// Base64-encode raw ssh-agent wire bytes for a
/// [`ControlMessage::SshAgentData`] frame. Standard alphabet with padding so any
/// base64 decoder (incl. the matching [`decode_ssh_agent_data`]) accepts it.
pub fn encode_ssh_agent_data(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// Decode the `data` field of a [`ControlMessage::SshAgentData`] frame back to
/// raw bytes, rejecting anything whose decoded length exceeds
/// [`SSH_AGENT_RELAY_MAX_FRAME`] *before* allocating the full buffer would be
/// unbounded — the cap is enforced on the decoded length. Returns the raw bytes
/// ready to write to an ssh-agent socket.
pub fn decode_ssh_agent_data(data: &str) -> Result<Vec<u8>, SshAgentDataError> {
    use base64::Engine as _;
    // Reject obviously-oversized payloads from the encoded length first (base64
    // inflates ~4/3, so an encoded string longer than ceil(cap*4/3) cannot decode
    // to <= cap), then enforce the exact cap on the decoded bytes.
    let max_encoded = SSH_AGENT_RELAY_MAX_FRAME
        .saturating_mul(4)
        .saturating_div(3)
        .saturating_add(4);
    if data.len() > max_encoded {
        return Err(SshAgentDataError::TooLarge);
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data.as_bytes())
        .map_err(|_| SshAgentDataError::InvalidBase64)?;
    if bytes.len() > SSH_AGENT_RELAY_MAX_FRAME {
        return Err(SshAgentDataError::TooLarge);
    }
    Ok(bytes)
}

/// Why a [`ControlMessage::SshAgentData`] payload could not be decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SshAgentDataError {
    /// The `data` field was not valid base64.
    InvalidBase64,
    /// The decoded payload exceeded [`SSH_AGENT_RELAY_MAX_FRAME`].
    TooLarge,
}

impl std::fmt::Display for SshAgentDataError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SshAgentDataError::InvalidBase64 => f.write_str("ssh-agent relay frame is not valid base64"),
            SshAgentDataError::TooLarge => write!(
                f,
                "ssh-agent relay frame exceeds {SSH_AGENT_RELAY_MAX_FRAME} bytes"
            ),
        }
    }
}

impl std::error::Error for SshAgentDataError {}

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
    ListDraftModels {
        provider: DraftProvider,
        settings: Option<DraftGenerationSettings>,
    },
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
    /// Fetch remote refs using the system `git` CLI.
    Fetch { worktree_id: WorktreeId },
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
    /// The auto commit-and-push **composite Job** (ADR 0013 amendment
    /// 2026-06-07): one daemon-owned chain of staging -> drafting -> committing
    /// -> pushing, so the chain survives GUI navigation/quit. Per-step progress
    /// rides [`Event::CompositeJobProgress`]; the [`Event::JobCompleted`] carries
    /// [`Response::CommitAndPushed`] on success or [`Response::CompositeJobFailed`]
    /// on a mid-chain failure (a draft failure aborts before any commit is made).
    CommitAndPush {
        worktree_id: WorktreeId,
        settings: Option<DraftGenerationSettings>,
    },
    /// The autonomous PR **composite Job** (ADR 0013 amendment 2026-06-07): a
    /// daemon-owned chain of pushing -> drafting (title/body) -> creating a
    /// GitHub **draft** PR. Completion carries the created PR URL inside
    /// [`Response::PullRequestCreated`]; the daemon never opens a browser (an
    /// attached GUI does that off the completion event).
    CreatePr {
        worktree_id: WorktreeId,
        base: Option<String>,
        settings: Option<DraftGenerationSettings>,
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

    /// List the directories under `path` on the daemon host so the GUI can render
    /// a remote directory browser before sending AddProject/CloneProject to this
    /// daemon (ADR 0014). `path: None` means the daemon user's home directory.
    /// `show_hidden` controls whether dot-prefixed entries are included (off by
    /// default in the browser). Replies with [`Response::DirectoryListing`]; an
    /// unreadable or nonexistent directory returns a [`ProtocolError`]
    /// (`NotFound`/`Unauthorized`) so the browser can render an error row. A fast
    /// synchronous filesystem read, not a Job.
    ListDirectory {
        path: Option<String>,
        show_hidden: bool,
    },

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
    /// Read a file-level diff for a path in a worktree. `staged == Some(true)`
    /// selects HEAD↔index; `Some(false)` selects index↔worktree. `None` keeps
    /// the legacy worktree-first, staged-fallback behavior for older clients.
    /// `ignore_whitespace == Some(true)` drops whitespace-only changes;
    /// `context_lines` overrides the surrounding context size (git default 3).
    /// Both are omitted by older clients and keep today's behavior when absent.
    GitDiff {
        worktree_id: WorktreeId,
        path: PathBuf,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        staged: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ignore_whitespace: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        context_lines: Option<u32>,
    },
    /// Read a page of the worktree's enriched `HEAD` commit log for the History
    /// view, newest first. `offset` skips that many commits; `limit` caps the
    /// page. Replies with [`Response::CommitLog`] (`has_more` flags more past
    /// the page). A fast synchronous git read, not a Job.
    GitLog {
        worktree_id: WorktreeId,
        limit: u32,
        offset: u32,
    },
    /// Read one commit's full diff — metadata plus per-file unified patches vs
    /// its first parent (the empty tree for a root commit) — in one round-trip.
    /// Replies with [`Response::CommitDiff`]. A fast synchronous git read.
    CommitDiff {
        worktree_id: WorktreeId,
        commit_id: String,
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
    ListDraftModels {
        provider: DraftProvider,
        settings: Option<DraftGenerationSettings>,
    },
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
    /// Fetch remote refs using the system `git` CLI.
    Fetch { worktree_id: WorktreeId },
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
    /// Run the auto commit-and-push composite Job for a worktree (the bare form
    /// of [`JobRequest::CommitAndPush`]; dispatches through the async Job path).
    CommitAndPush {
        worktree_id: WorktreeId,
        settings: Option<DraftGenerationSettings>,
    },
    /// Run the autonomous PR composite Job for a worktree (the bare form of
    /// [`JobRequest::CreatePr`]; dispatches through the async Job path).
    CreatePr {
        worktree_id: WorktreeId,
        base: Option<String>,
        settings: Option<DraftGenerationSettings>,
    },
    /// Return a worktree's currently in-flight **Jobs** (ADR 0008 + ADR 0013
    /// amendment), so a (re)attaching GUI can restore the Composer/button state
    /// for chains already running in the daemon. A fast in-memory read, not a
    /// Job; Jobs remain ephemeral across daemon restarts.
    ActiveJobs { worktree_id: WorktreeId },

    /// Install/merge known-agent hooks in the target worktree.
    InstallAgentHooks { worktree_id: WorktreeId },
    /// Hook helper report: map a known agent hook event to Hitch Agent State.
    ReportAgentState {
        agent: KnownAgent,
        state: Option<AgentState>,
        session_id: Option<SessionId>,
        cwd: Option<PathBuf>,
        detail: Option<String>,
        /// Agent-native session/run id from hook payloads (Claude Code's
        /// `session_id`), distinct from Hitch's PTY [`SessionId`]. Missing for
        /// older helpers and non-agent manual invocations.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent_run_id: Option<String>,
    },
    /// Hook helper **identity announce** (ADR 0011 amendment 2026-06-05): an
    /// agent's `SessionStart` declares *which* agent now runs in a session so the
    /// Session mark can render before the first prompt. Identity is **not** a
    /// non-null state: this shape carries no `state` field, so it can never be
    /// confused with a [`ReportAgentState`] whose `state: None` *clears*
    /// identity. A new identity/run id is still a process boundary; the daemon may
    /// clear stale visible state while keeping `agent: Some(..)`. The late-arrival
    /// guard does not block announces; exit-to-`None` clears identity with state.
    AnnounceAgent {
        agent: KnownAgent,
        session_id: Option<SessionId>,
        cwd: Option<PathBuf>,
        /// Agent-native session/run id from hook payloads, used only to reject
        /// stale lifecycle hooks that arrive after a newer run announced itself.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent_run_id: Option<String>,
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

    /// Begin a file-drop **upload** into a remote Session (issue #31, ADR 0014).
    /// The daemon validates the session exists, creates its per-session upload dir
    /// `<data-dir>/uploads/<session-id>/`, resolves a collision-suffixed final
    /// name AT BEGIN time (reserving it by creating the file), and replies with
    /// [`Response::UploadStarted`]. `file_name` is a bare name — the daemon rejects
    /// path separators and `..`. `total_bytes` is advisory for progress only.
    BeginUpload {
        session_id: SessionId,
        file_name: String,
        total_bytes: u64,
    },
    /// Announce that a raw PTY-style frame of `byte_count` upload bytes follows
    /// this request, appended to the upload identified by `upload_id`. The daemon
    /// replies [`Response::Ack`] per chunk (sequential, windowed by the GUI). The
    /// frame uses the same length-prefixed framing as [`SendSessionInput`].
    UploadChunk {
        upload_id: UploadId,
        byte_count: u32,
    },
    /// Finish an upload: the daemon flushes/closes the file and replies with
    /// [`Response::UploadFinished`] carrying the ACTUAL final absolute remote path
    /// (post collision-suffixing) for the GUI to insert at the prompt.
    FinishUpload { upload_id: UploadId },
    /// Abort an in-flight upload: the daemon deletes the partial file and replies
    /// [`Response::Ack`]. Sent on user cancellation before paths are inserted.
    AbortUpload { upload_id: UploadId },
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
            JobRequest::ListDraftModels { provider, settings } => {
                Request::ListDraftModels { provider, settings }
            }
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
            JobRequest::Fetch { worktree_id } => Request::Fetch { worktree_id },
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
            JobRequest::CommitAndPush {
                worktree_id,
                settings,
            } => Request::CommitAndPush {
                worktree_id,
                settings,
            },
            JobRequest::CreatePr {
                worktree_id,
                base,
                settings,
            } => Request::CreatePr {
                worktree_id,
                base,
                settings,
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
            Request::ListDraftModels { provider, settings } => {
                Ok(JobRequest::ListDraftModels { provider, settings })
            }
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
            Request::Fetch { worktree_id } => Ok(JobRequest::Fetch { worktree_id }),
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
            Request::CommitAndPush {
                worktree_id,
                settings,
            } => Ok(JobRequest::CommitAndPush {
                worktree_id,
                settings,
            }),
            Request::CreatePr {
                worktree_id,
                base,
                settings,
            } => Ok(JobRequest::CreatePr {
                worktree_id,
                base,
                settings,
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
        /// The daemon host's OS family (issue #31), so the GUI quotes inserted
        /// remote upload paths for the right shell. `#[serde(default)]` to Unix
        /// keeps an old daemon's Hello (no field) decodable during rolling
        /// upgrades; the version check already gates real upload use.
        #[serde(default = "os_family_default")]
        os_family: OsFamily,
        /// The daemon's own executable path, captured from `current_exe()` at
        /// startup (approach C, ADR 0014 amendment). The client caches it and
        /// re-invokes this exact binary on reconnect — shell-free and PATH-free.
        /// `#[serde(default)]` to `None` keeps an old daemon's Hello (no field)
        /// decodable during rolling upgrades; the client then falls back to its
        /// candidate-path probe.
        #[serde(default)]
        exe_path: Option<String>,
    },
    /// Command succeeded and has no body.
    Ack,
    Projects {
        projects: Vec<Project>,
    },
    /// A directory listing for the remote folder browser (reply to
    /// [`Request::ListDirectory`]). `path` is the absolute directory that was
    /// listed (the home directory when the request omitted a path), `parent` is
    /// its parent directory or `None` at the filesystem root, `home` is the
    /// daemon user's home directory (for the browser's Home control), and
    /// `entries` are its child directories sorted case-insensitively by name.
    DirectoryListing {
        path: String,
        parent: Option<String>,
        home: String,
        entries: Vec<DirEntry>,
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
        agent: Option<KnownAgent>,
        agent_state: Option<AgentState>,
        agent_detail: Option<String>,
        /// Whether the session's PTY produced output within the last ~N seconds
        /// (the daemon-computed output-activity gate, ADR 0011 amendment
        /// 2026-06-05). Defaults to `false` for rolling upgrades. A newly attached
        /// client combines `running ∧ output_active` for the `WORKING` display
        /// word; carrying the current value here keeps it from assuming wrongly
        /// before the next edge-triggered [`Event::OutputActive`] arrives.
        #[serde(default)]
        output_active: bool,
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
    /// A page of the History commit log (reply to [`Request::GitLog`]).
    /// `has_more` is `true` when commits remain past this page.
    CommitLog {
        commits: Vec<CommitInfo>,
        has_more: bool,
    },
    /// One commit's metadata and per-file diff (reply to [`Request::CommitDiff`]).
    CommitDiff {
        meta: CommitMeta,
        files: Vec<CommitFileDiff>,
    },
    PullRequestCreated {
        url: String,
    },
    /// Terminal payload of a successful `commit-and-push` composite Job, carried
    /// inside [`Event::JobCompleted`]. The GUI builds its completion toast from
    /// this (subject, short sha, pushed commit count, file count).
    CommitAndPushed {
        result: CommitAndPushResult,
    },
    /// Terminal payload of a composite Job that failed mid-chain, carried inside
    /// [`Event::JobCompleted`]. `failed_step` names the step that failed and
    /// `reason` is its error; `result` carries whatever prior steps completed
    /// (e.g. the commit that landed before a push failure) so the GUI can report
    /// the partial chain accurately.
    CompositeJobFailed {
        kind: CompositeJobKind,
        failed_step: CompositeStep,
        reason: String,
        result: CompositeJobResult,
    },
    /// A worktree's in-flight composite Jobs (reply to [`Request::ActiveJobs`]),
    /// so a (re)attaching GUI restores chain/button state. Empty when none run.
    ActiveJobs {
        jobs: Vec<ActiveJobInfo>,
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
    /// A file-drop upload was accepted (reply to [`Request::BeginUpload`], issue
    /// #31). `upload_id` keys the following chunks/finish/abort; `final_name` is
    /// the collision-suffixed name the daemon reserved (e.g. `file-1.txt`), echoed
    /// for diagnostics — the GUI inserts the path from [`UploadFinished`].
    UploadStarted {
        upload_id: UploadId,
        final_name: String,
    },
    /// An upload finished (reply to [`Request::FinishUpload`]). `remote_path` is
    /// the ACTUAL final absolute path on the daemon host, post collision-suffixing,
    /// for the GUI to quote and insert at the Session prompt.
    UploadFinished {
        remote_path: String,
    },
    /// Command failed. Kept in-band so clients can correlate by request id.
    Error {
        error: ProtocolError,
    },
}

/// Default OS family for an old daemon's Hello that predates the field (Unix —
/// the historical-only platform before Windows support). Real upload use is gated
/// by the protocol-version check, so this only affects decoding a stale Hello.
fn os_family_default() -> OsFamily {
    OsFamily::Unix
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
        agent: Option<KnownAgent>,
        agent_state: Option<AgentState>,
        agent_detail: Option<String>,
        /// Current output-activity gate value at attach time (ADR 0011 amendment
        /// 2026-06-05); see [`Response::SessionOpened::output_active`]. Defaults
        /// to `false` for rolling upgrades.
        #[serde(default)]
        output_active: bool,
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
    /// Daemon-owned Agent identity + state for a session. Identity and state
    /// clear INDEPENDENTLY (ADR 0011 amendment 2026-06-05): `agent: None` is
    /// the identity clear (exit-to-`None` reverts the Session mark to shell),
    /// while an identity announce carries `agent: Some(..)` with the session's
    /// current — typically still `None` pre-prompt — state. A null `state`
    /// alone must never be read as an identity clear, or the announce would be
    /// indistinguishable from the exit clear on the wire.
    AgentState {
        session_id: Option<SessionId>,
        worktree_id: Option<WorktreeId>,
        agent: Option<KnownAgent>,
        state: Option<AgentState>,
        detail: Option<String>,
    },
    /// Edge-triggered per-session output-activity transition (ADR 0011 amendment
    /// 2026-06-05). The daemon sees every PTY output frame regardless of GUI
    /// attachment; it broadcasts `active: true` on the rising edge (first frame
    /// after a quiet period) and `active: false` on the falling edge (~N seconds
    /// with no output). It is a *transition*, never a per-frame ping or a
    /// timestamp — while a session stays active no further events fire. This
    /// gates the `WORKING` display word (`running ∧ output-active`); it watches
    /// *whether* bytes flow, never what they say, so it does not revisit the
    /// no-text-inference rule (ADR 0011 / ADR 0002). `worktree_id` mirrors
    /// [`Event::AgentState`] so a worktree-scoped client can route it without a
    /// session lookup; it is `None` for project-root sessions.
    OutputActive {
        session_id: SessionId,
        worktree_id: Option<WorktreeId>,
        active: bool,
    },
    WorktreeDirty {
        worktree_id: WorktreeId,
        dirty: bool,
    },
    WorktreeUpdated {
        worktree: Worktree,
    },
    WorktreeRemoved {
        worktree_id: WorktreeId,
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
    /// A **composite Job** (ADR 0013 amendment) advanced to a new step, or that
    /// step finished. Broadcast per step start/finish so an attached GUI morphs
    /// the action button's label in place (staging -> drafting -> committing ->
    /// pushing for `commit-and-push`; pushing -> drafting -> creating-pr for
    /// `create-pr`). `worktree_id` lets a worktree-scoped client route it without
    /// a Job lookup; `kind` and `step` identify the chain and its current rung,
    /// and `phase` distinguishes the step starting from it finishing.
    CompositeJobProgress {
        job_id: JobId,
        worktree_id: WorktreeId,
        kind: CompositeJobKind,
        step: CompositeStep,
        phase: StepPhase,
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

/// Which daemon-owned **composite Job** chain a step/result belongs to (ADR 0013
/// amendment 2026-06-07). The string tags match the UI-facing job kinds carried
/// by [`Event::JobProgress::kind`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CompositeJobKind {
    /// staging -> drafting -> committing -> pushing.
    CommitAndPush,
    /// pushing -> drafting -> creating the GitHub draft PR.
    CreatePr,
}

/// One rung of a composite Job chain, reported on [`Event::CompositeJobProgress`].
/// The two chains use overlapping subsets: `commit-and-push` runs `Staging`,
/// `Drafting`, `Committing`, `Pushing`; `create-pr` runs `Pushing`, `Drafting`,
/// `CreatingPr`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CompositeStep {
    Staging,
    Drafting,
    Committing,
    Pushing,
    CreatingPr,
}

/// Whether a [`CompositeStep`] is starting or has finished.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StepPhase {
    Started,
    Finished,
}

/// Result of a successful `commit-and-push` composite Job. The GUI's completion
/// toast reads all four fields (subject - short sha - pushed count - file count).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitAndPushResult {
    /// The committed subject line.
    pub subject: String,
    /// Short (7-char) SHA of the new commit.
    pub short_sha: String,
    /// Number of commits pushed to the remote in the push step.
    pub pushed_commits: u32,
    /// Number of files in the commit.
    pub file_count: u32,
}

/// Whatever a composite Job completed before failing, so a failure can report
/// prior steps' results intact (ADR 0013 amendment): the commit that landed
/// before a push failure. `None` when the chain aborted before producing
/// anything (e.g. a draft-generation failure that aborts before any commit).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct CompositeJobResult {
    /// The commit produced by a `commit-and-push` chain before a later step
    /// failed (the commit stays — only the push failed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<CommitAndPushResult>,
}

/// One in-flight composite Job for a worktree (entry in [`Response::ActiveJobs`]).
/// Carries enough for a re-attaching GUI to restore the action button's state:
/// which chain, which step it is currently on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveJobInfo {
    pub job_id: JobId,
    pub worktree_id: WorktreeId,
    pub kind: CompositeJobKind,
    /// The step the chain is currently executing.
    pub step: CompositeStep,
}

/// One child directory in a [`Response::DirectoryListing`] (the remote folder
/// browser's row data). The browser is folders-first and only folders are
/// selectable, so the daemon lists directories only — files never appear. `path`
/// is the absolute path of the entry so the GUI can navigate or AddProject it
/// without re-joining paths (the GUI never maps remote paths onto local paths).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirEntry {
    pub name: String,
    pub path: String,
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
    /// Full SHA of the current `HEAD` commit, or `None` on an unborn HEAD. The
    /// History view refetches its log when this changes (~1s status backbone).
    /// `#[serde(default)]` keeps older daemons/clients decodable during rolling
    /// upgrades, matching the other added GitStatus fields.
    #[serde(default)]
    pub head_commit_id: Option<String>,
    /// The daemon-resolved "base branch" for this worktree — the branch checked
    /// out in the project's main worktree (falling back to the repo's default
    /// branch), or `None` when neither resolves. This is the SINGLE definition of
    /// the base convention: the History `ahead_of_base` markers and the
    /// frontend's PR-base default / "from {base}" labels both read it from here
    /// instead of each recomputing it. `#[serde(default)]` keeps older
    /// daemons/clients decodable during rolling upgrades, matching the other
    /// added GitStatus fields.
    #[serde(default)]
    pub base_branch: Option<String>,
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
    /// Added/deleted line counts for this path (the side shown: staged counts
    /// when staged, worktree counts otherwise). `#[serde(default)]` keeps older
    /// daemons/clients decodable during rolling upgrades, matching GitStatus.
    #[serde(default)]
    pub additions: u32,
    #[serde(default)]
    pub deletions: u32,
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

/// One commit in a [`Response::CommitLog`] page (the History view's row data).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitInfo {
    /// Full commit SHA.
    pub id: String,
    /// Commit summary (first line), `None` when absent.
    pub summary: Option<String>,
    /// Commit message body (after the summary), `None` when absent.
    pub body: Option<String>,
    pub author: Option<String>,
    /// Author time in unix seconds.
    pub time: i64,
    /// Whether this commit has more than one parent (carries a merge badge).
    pub is_merge: bool,
    /// Whether this commit is ahead of the base branch (branch-work marker).
    /// Always `false` when no base is resolvable (e.g. the main worktree).
    pub ahead_of_base: bool,
    /// Added/deleted line totals vs the first parent (empty tree for a root).
    pub additions: u32,
    pub deletions: u32,
}

/// Metadata header for a [`Response::CommitDiff`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitMeta {
    /// Full commit SHA.
    pub id: String,
    pub summary: Option<String>,
    pub body: Option<String>,
    pub author: Option<String>,
    /// Author time in unix seconds.
    pub time: i64,
    pub is_merge: bool,
    pub additions: u32,
    pub deletions: u32,
}

/// One file's unified diff within a [`Response::CommitDiff`]. Mirrors the
/// working-tree per-file diff shape (path + status + patch text) so the frontend
/// can reuse its diff renderer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitFileDiff {
    pub path: PathBuf,
    pub status: FileStatus,
    pub diff: String,
}

/// Client-selected Draft Generator provider settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftGenerationSettings {
    pub provider: DraftProvider,
    pub model: Option<String>,
    pub claude_path: Option<PathBuf>,
    pub codex_path: Option<PathBuf>,
    /// Optional Draft Instructions appended to the built-in commit prompt as an
    /// extra block; never replaces the prompt or its JSON output contract (ADR
    /// 0007 amendment 2026-06-07). `#[serde(default)]` keeps older clients that
    /// omit the field decodable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit_instructions: Option<String>,
    /// Optional Draft Instructions appended to the built-in pull-request prompt;
    /// same append-never-replace contract as `commit_instructions`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr_instructions: Option<String>,
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
        // An old daemon's status without the head id decodes to None.
        assert_eq!(status.head_commit_id, None);
        // An old daemon's status without the daemon-resolved base also decodes to
        // None — the frontend then falls back to its worktree-derived base for the
        // rolling-upgrade window.
        assert_eq!(status.base_branch, None);
    }

    #[test]
    fn git_status_carries_daemon_resolved_base_branch_as_contract() {
        let (_, worktree_id, _) = ids();
        let status = GitStatus {
            worktree_id,
            branch: "feature".into(),
            dirty: false,
            ahead: 2,
            behind: 0,
            additions: 0,
            deletions: 0,
            head_commit_id: Some("a".repeat(40)),
            base_branch: Some("main".into()),
            files: vec![],
        };
        let response = Response::GitStatus { status };
        let value: serde_json::Value = serde_json::to_value(&response).unwrap();
        assert_eq!(value["type"], "git-status");
        // The base rides on the status backbone under a snake_case key so the
        // frontend's `defaultBase` can read it without recomputing the convention.
        assert_eq!(value["status"]["base_branch"], "main");
        let back: Response = serde_json::from_value(value).unwrap();
        assert_eq!(response, back);

        // A main worktree whose own branch is the base resolves to null; that is a
        // distinct, intentional shape (no cross-branch default), not the
        // rolling-upgrade missing-field case.
        let no_base = GitStatus {
            worktree_id,
            branch: "main".into(),
            dirty: false,
            ahead: 0,
            behind: 0,
            additions: 0,
            deletions: 0,
            head_commit_id: Some("a".repeat(40)),
            base_branch: None,
            files: vec![],
        };
        let value: serde_json::Value = serde_json::to_value(&no_base).unwrap();
        assert!(value["base_branch"].is_null());
    }

    #[test]
    fn git_log_request_and_commit_log_response_round_trip_as_contract() {
        let (_, worktree_id, _) = ids();

        let request = Request::GitLog {
            worktree_id,
            limit: 100,
            offset: 200,
        };
        let value: serde_json::Value = serde_json::to_value(&request).unwrap();
        assert_eq!(value["type"], "git-log");
        assert_eq!(value["worktree_id"], worktree_id.to_string());
        assert_eq!(value["limit"], 100);
        assert_eq!(value["offset"], 200);
        let back: Request = serde_json::from_value(value).unwrap();
        assert_eq!(request, back);

        let response = Response::CommitLog {
            commits: vec![CommitInfo {
                id: "a".repeat(40),
                summary: Some("feat: add proto".into()),
                body: None,
                author: Some("Hitch Test".into()),
                time: 1_700_000_000,
                is_merge: true,
                ahead_of_base: true,
                additions: 5,
                deletions: 1,
            }],
            has_more: false,
        };
        let value: serde_json::Value = serde_json::to_value(&response).unwrap();
        assert_eq!(value["type"], "commit-log");
        assert_eq!(value["has_more"], false);
        assert_eq!(value["commits"][0]["is_merge"], true);
        assert_eq!(value["commits"][0]["ahead_of_base"], true);
        assert!(value["commits"][0]["body"].is_null());
        let back: Response = serde_json::from_value(value).unwrap();
        assert_eq!(response, back);
    }

    #[test]
    fn commit_diff_request_and_response_round_trip_as_contract() {
        let (_, worktree_id, _) = ids();

        let request = Request::CommitDiff {
            worktree_id,
            commit_id: "a".repeat(40),
        };
        let value: serde_json::Value = serde_json::to_value(&request).unwrap();
        assert_eq!(value["type"], "commit-diff");
        assert_eq!(value["commit_id"], "a".repeat(40));
        let back: Request = serde_json::from_value(value).unwrap();
        assert_eq!(request, back);

        let response = Response::CommitDiff {
            meta: CommitMeta {
                id: "a".repeat(40),
                summary: Some("feat: add proto".into()),
                body: Some("Body".into()),
                author: Some("Hitch Test".into()),
                time: 1_700_000_000,
                is_merge: false,
                additions: 5,
                deletions: 1,
            },
            files: vec![CommitFileDiff {
                path: "src/lib.rs".into(),
                status: FileStatus::Added,
                diff: "diff --git a/src/lib.rs b/src/lib.rs".into(),
            }],
        };
        let value: serde_json::Value = serde_json::to_value(&response).unwrap();
        assert_eq!(value["type"], "commit-diff");
        assert_eq!(value["meta"]["id"], "a".repeat(40));
        // The per-file diff reuses the FileStatus enum so the frontend renderer
        // can be shared with working-tree diffs.
        assert_eq!(value["files"][0]["status"], "added");
        let back: Response = serde_json::from_value(value).unwrap();
        assert_eq!(response, back);
    }

    #[test]
    fn changed_file_deserializes_without_line_counts_for_rolling_upgrades() {
        let json = r#"{"path":"src/lib.rs","status":"modified","staged":true}"#;
        let file: ChangedFile = serde_json::from_str(json).unwrap();
        assert_eq!(file.additions, 0);
        assert_eq!(file.deletions, 0);

        let with_counts = r#"{"path":"src/lib.rs","status":"modified","staged":true,"additions":7,"deletions":2}"#;
        let file: ChangedFile = serde_json::from_str(with_counts).unwrap();
        assert_eq!(file.additions, 7);
        assert_eq!(file.deletions, 2);
    }

    #[test]
    fn git_diff_deserializes_without_side_for_rolling_upgrades() {
        let (_, worktree_id, _) = ids();
        let json =
            format!(r#"{{"type":"git-diff","worktree_id":"{worktree_id}","path":"src/lib.rs"}}"#);
        let request: Request = serde_json::from_str(&json).unwrap();

        assert_eq!(
            request,
            Request::GitDiff {
                worktree_id,
                path: "src/lib.rs".into(),
                staged: None,
                ignore_whitespace: None,
                context_lines: None,
            }
        );
    }

    #[test]
    fn git_diff_carries_whitespace_and_context_view_options() {
        let (_, worktree_id, _) = ids();
        let json = format!(
            r#"{{"type":"git-diff","worktree_id":"{worktree_id}","path":"src/lib.rs","ignore_whitespace":true,"context_lines":10}}"#
        );
        let request: Request = serde_json::from_str(&json).unwrap();

        assert_eq!(
            request,
            Request::GitDiff {
                worktree_id,
                path: "src/lib.rs".into(),
                staged: None,
                ignore_whitespace: Some(true),
                context_lines: Some(10),
            }
        );

        // The same field names round-trip back out (snake_case, like the rest of
        // the request payload).
        let value = serde_json::to_value(&request).unwrap();
        assert_eq!(value["ignore_whitespace"], true);
        assert_eq!(value["context_lines"], 10);
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
    fn agent_state_null_clears_request_and_event_state() {
        let (_, worktree_id, session_id) = ids();
        let request = Request::ReportAgentState {
            agent: KnownAgent::ClaudeCode,
            state: None,
            session_id: Some(session_id),
            cwd: Some("/repo".into()),
            detail: None,
            agent_run_id: None,
        };
        let value: serde_json::Value = serde_json::to_value(&request).unwrap();
        assert_eq!(value["type"], "report-agent-state");
        assert!(value["state"].is_null());
        let back: Request = serde_json::from_value(value).unwrap();
        assert_eq!(request, back);

        // The exit-to-`None` clear event carries `agent: null` too: identity
        // clears on a null AGENT, never on the null state alone (the identity
        // announce also broadcasts a null pre-prompt state).
        let event = Event::AgentState {
            session_id: Some(session_id),
            worktree_id: Some(worktree_id),
            agent: None,
            state: None,
            detail: None,
        };
        let value: serde_json::Value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["type"], "agent-state");
        assert!(value["state"].is_null());
        assert!(value["agent"].is_null());
        let back: Event = serde_json::from_value(value).unwrap();
        assert_eq!(event, back);

        // The identity announce broadcast: agent known, state still null. Same
        // null state as the clear above — only the `agent` field tells them
        // apart on the wire.
        let announce_event = Event::AgentState {
            session_id: Some(session_id),
            worktree_id: Some(worktree_id),
            agent: Some(KnownAgent::ClaudeCode),
            state: None,
            detail: None,
        };
        let value: serde_json::Value = serde_json::to_value(&announce_event).unwrap();
        assert_eq!(value["agent"], "claude-code");
        assert!(value["state"].is_null());
        let back: Event = serde_json::from_value(value).unwrap();
        assert_eq!(announce_event, back);
    }

    #[test]
    fn announce_agent_round_trips_and_is_distinct_from_state_report() {
        let (_, _, session_id) = ids();

        // The identity announce round-trips like any other request and serializes
        // with its own kebab-case tag.
        let announce = Request::AnnounceAgent {
            agent: KnownAgent::ClaudeCode,
            session_id: Some(session_id),
            cwd: Some("/repo".into()),
            agent_run_id: Some("claude-run-1".into()),
        };
        let value: serde_json::Value = serde_json::to_value(&announce).unwrap();
        assert_eq!(value["type"], "announce-agent");
        assert_eq!(value["agent"], "claude-code");
        assert_eq!(value["session_id"], session_id.to_string());
        assert_eq!(value["cwd"], "/repo");
        // Identity is NOT state: the wire shape has no `state` field at all, so it
        // can never be confused with a `state: None` clear.
        assert!(value.get("state").is_none());
        let back: Request = serde_json::from_value(value).unwrap();
        assert_eq!(announce, back);

        // A `state: None` clear is a DIFFERENT report shape with a different tag.
        let clear = Request::ReportAgentState {
            agent: KnownAgent::ClaudeCode,
            state: None,
            session_id: Some(session_id),
            cwd: Some("/repo".into()),
            detail: None,
            agent_run_id: Some("claude-run-1".into()),
        };
        let clear_value: serde_json::Value = serde_json::to_value(&clear).unwrap();
        assert_eq!(clear_value["type"], "report-agent-state");
        assert!(clear_value["state"].is_null());

        // The two shapes are not interchangeable: deserializing an announce as a
        // state report (and vice versa) fails on the tag, so a clear can never be
        // misread as an announce.
        assert_ne!(value_tag(&announce), value_tag(&clear));
        let announce_json = serde_json::to_string(&announce).unwrap();
        let clear_json = serde_json::to_string(&clear).unwrap();
        let reparsed_announce: Request = serde_json::from_str(&announce_json).unwrap();
        let reparsed_clear: Request = serde_json::from_str(&clear_json).unwrap();
        assert!(matches!(reparsed_announce, Request::AnnounceAgent { .. }));
        assert!(matches!(reparsed_clear, Request::ReportAgentState { .. }));
    }

    #[test]
    fn upload_messages_round_trip_as_contract() {
        let (_, _, session_id) = ids();

        let begin = Request::BeginUpload {
            session_id,
            file_name: "report.pdf".into(),
            total_bytes: 2048,
        };
        let value: serde_json::Value = serde_json::to_value(&begin).unwrap();
        assert_eq!(value["type"], "begin-upload");
        assert_eq!(value["file_name"], "report.pdf");
        assert_eq!(value["total_bytes"], 2048);
        let back: Request = serde_json::from_value(value).unwrap();
        assert_eq!(begin, back);

        let chunk = Request::UploadChunk {
            upload_id: "upload-1".into(),
            byte_count: 4096,
        };
        let value: serde_json::Value = serde_json::to_value(&chunk).unwrap();
        assert_eq!(value["type"], "upload-chunk");
        assert_eq!(value["byte_count"], 4096);
        let back: Request = serde_json::from_value(value).unwrap();
        assert_eq!(chunk, back);

        let started = Response::UploadStarted {
            upload_id: "upload-1".into(),
            final_name: "file-1.txt".into(),
        };
        let value: serde_json::Value = serde_json::to_value(&started).unwrap();
        assert_eq!(value["type"], "upload-started");
        assert_eq!(value["final_name"], "file-1.txt");
        let back: Response = serde_json::from_value(value).unwrap();
        assert_eq!(started, back);

        let finished = Response::UploadFinished {
            remote_path: "/home/dev/.hitch/uploads/abc/file-1.txt".into(),
        };
        let value: serde_json::Value = serde_json::to_value(&finished).unwrap();
        assert_eq!(value["type"], "upload-finished");
        assert_eq!(value["remote_path"], "/home/dev/.hitch/uploads/abc/file-1.txt");
        let back: Response = serde_json::from_value(value).unwrap();
        assert_eq!(finished, back);
    }

    #[test]
    fn hello_reports_os_family_and_defaults_to_unix_for_rolling_upgrades() {
        let hello = Response::Hello {
            protocol_version: PROTOCOL_VERSION,
            daemon_pid: 7,
            os_family: OsFamily::Windows,
            exe_path: None,
        };
        let value: serde_json::Value = serde_json::to_value(&hello).unwrap();
        assert_eq!(value["os_family"], "windows");
        let back: Response = serde_json::from_value(value).unwrap();
        assert_eq!(hello, back);

        // An old daemon's Hello without the field decodes with os_family = unix.
        let legacy = serde_json::json!({
            "type": "hello",
            "protocol_version": PROTOCOL_VERSION,
            "daemon_pid": 7,
        });
        let back: Response = serde_json::from_value(legacy).unwrap();
        let Response::Hello { os_family, .. } = back else {
            panic!("expected hello");
        };
        assert_eq!(os_family, OsFamily::Unix);
    }

    #[test]
    fn hello_carries_exe_path_and_decodes_without_it_for_rolling_upgrades() {
        // A new daemon's Hello carrying its startup exe path round-trips and the
        // path is readable for the client to cache (approach C, ADR 0014).
        let hello = Response::Hello {
            protocol_version: PROTOCOL_VERSION,
            daemon_pid: 7,
            os_family: OsFamily::Unix,
            exe_path: Some("/home/dev/.local/bin/hitch".into()),
        };
        let value: serde_json::Value = serde_json::to_value(&hello).unwrap();
        assert_eq!(value["exe_path"], "/home/dev/.local/bin/hitch");
        let back: Response = serde_json::from_value(value).unwrap();
        assert_eq!(hello, back);

        // An old daemon at v26 omits `exe_path`; it must still decode (to `None`)
        // so the client can fall back to its candidate-path probe.
        let legacy = serde_json::json!({
            "type": "hello",
            "protocol_version": PROTOCOL_VERSION,
            "daemon_pid": 7,
            "os_family": "unix",
        });
        let back: Response = serde_json::from_value(legacy).unwrap();
        let Response::Hello { exe_path, .. } = back else {
            panic!("expected hello");
        };
        assert_eq!(exe_path, None);
    }

    fn value_tag(request: &Request) -> String {
        serde_json::to_value(request).unwrap()["type"]
            .as_str()
            .unwrap()
            .to_string()
    }

    #[test]
    fn session_opened_replay_carries_current_agent_state() {
        let (_, worktree_id, session_id) = ids();
        let session = sample_session(worktree_id, session_id);

        let response = Response::SessionOpened {
            session: session.clone(),
            agent: Some(KnownAgent::ClaudeCode),
            agent_state: Some(AgentState::NeedsApproval),
            agent_detail: Some("permission prompt".into()),
            output_active: true,
        };
        let value: serde_json::Value = serde_json::to_value(&response).unwrap();
        assert_eq!(value["type"], "session-opened");
        assert_eq!(value["agent"], "claude-code");
        assert_eq!(value["agent_state"], "needs-approval");
        assert_eq!(value["agent_detail"], "permission prompt");
        assert_eq!(value["output_active"], true);
        let back: Response = serde_json::from_value(value).unwrap();
        assert_eq!(response, back);

        let event = Event::SessionOpened {
            session,
            agent: None,
            agent_state: None,
            agent_detail: None,
            output_active: false,
        };
        let value: serde_json::Value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["type"], "session-opened");
        assert!(value["agent"].is_null());
        assert!(value["agent_state"].is_null());
        assert!(value["agent_detail"].is_null());
        assert_eq!(value["output_active"], false);
        let back: Event = serde_json::from_value(value).unwrap();
        assert_eq!(event, back);

        // Rolling upgrade: an old daemon's session-opened without `output_active`
        // deserializes with the field defaulted to false.
        let legacy = serde_json::json!({
            "type": "session-opened",
            "session": serde_json::to_value(sample_session(worktree_id, session_id)).unwrap(),
            "agent": null,
            "agent_state": null,
            "agent_detail": null,
        });
        let back: Event = serde_json::from_value(legacy).unwrap();
        let Event::SessionOpened { output_active, .. } = back else {
            panic!("expected session-opened event");
        };
        assert!(!output_active);
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
    fn protocol_version_tracks_draft_generator_settings_wire_contract() {
        let (_, worktree_id, _) = ids();
        let settings = DraftGenerationSettings {
            provider: DraftProvider::Claude,
            model: Some("sonnet".into()),
            claude_path: Some(r"C:\Program Files\Claude\claude.exe".into()),
            codex_path: None,
            commit_instructions: Some("Reference the ticket id".into()),
            pr_instructions: None,
        };

        let request = Request::ListDraftModels {
            provider: DraftProvider::Claude,
            settings: Some(settings.clone()),
        };
        let value: serde_json::Value = serde_json::to_value(&request).unwrap();
        assert_eq!(value["type"], "list-draft-models");
        assert_eq!(value["provider"], "claude");
        assert_eq!(value["settings"]["provider"], "claude");
        assert_eq!(value["settings"]["model"], "sonnet");
        assert_eq!(
            value["settings"]["claude_path"],
            r"C:\Program Files\Claude\claude.exe"
        );
        assert!(value["settings"]["codex_path"].is_null());
        // Draft Instructions ride the same settings payload; an unset field is
        // omitted entirely so older daemons still decode it.
        assert_eq!(
            value["settings"]["commit_instructions"],
            "Reference the ticket id"
        );
        assert!(value["settings"].get("pr_instructions").is_none());
        let back: Request = serde_json::from_value(value).unwrap();

        let job = JobRequest::try_from(request.clone()).unwrap();
        assert_eq!(
            job,
            JobRequest::ListDraftModels {
                provider: DraftProvider::Claude,
                settings: Some(settings.clone()),
            }
        );
        assert_eq!(Request::from(job), request);
        assert_eq!(request, back);

        let request = Request::GenerateCommitDraft {
            worktree_id,
            settings: Some(settings),
        };
        let value: serde_json::Value = serde_json::to_value(&request).unwrap();
        assert_eq!(
            value["settings"]["claude_path"],
            r"C:\Program Files\Claude\claude.exe"
        );
        let back: Request = serde_json::from_value(value).unwrap();
        assert_eq!(request, back);

        assert_eq!(PROTOCOL_VERSION, 30);
    }

    #[test]
    fn list_directory_request_and_directory_listing_response_round_trip_as_contract() {
        // The remote folder browser request: `path: None` means the daemon user's
        // home directory, `show_hidden` defaults off in the browser.
        let request = Request::ListDirectory {
            path: None,
            show_hidden: false,
        };
        let value: serde_json::Value = serde_json::to_value(&request).unwrap();
        assert_eq!(value["type"], "list-directory");
        assert!(value["path"].is_null());
        assert_eq!(value["show_hidden"], false);
        let back: Request = serde_json::from_value(value).unwrap();
        assert_eq!(request, back);

        let jump = Request::ListDirectory {
            path: Some("/home/dev/code".into()),
            show_hidden: true,
        };
        let value: serde_json::Value = serde_json::to_value(&jump).unwrap();
        assert_eq!(value["path"], "/home/dev/code");
        assert_eq!(value["show_hidden"], true);
        let back: Request = serde_json::from_value(value).unwrap();
        assert_eq!(jump, back);

        // The listing reply carries the absolute path, its parent (null at the
        // filesystem root), the home directory for the Home control, and
        // folders-only entries with their absolute paths.
        let response = Response::DirectoryListing {
            path: "/home/dev".into(),
            parent: Some("/home".into()),
            home: "/home/dev".into(),
            entries: vec![DirEntry {
                name: "code".into(),
                path: "/home/dev/code".into(),
            }],
        };
        let value: serde_json::Value = serde_json::to_value(&response).unwrap();
        assert_eq!(value["type"], "directory-listing");
        assert_eq!(value["path"], "/home/dev");
        assert_eq!(value["parent"], "/home");
        assert_eq!(value["home"], "/home/dev");
        assert_eq!(value["entries"][0]["name"], "code");
        assert_eq!(value["entries"][0]["path"], "/home/dev/code");
        let back: Response = serde_json::from_value(value).unwrap();
        assert_eq!(response, back);

        // The filesystem-root case carries a null parent.
        let root = Response::DirectoryListing {
            path: "/".into(),
            parent: None,
            home: "/home/dev".into(),
            entries: vec![],
        };
        let value: serde_json::Value = serde_json::to_value(&root).unwrap();
        assert!(value["parent"].is_null());
    }

    #[test]
    fn output_active_event_serializes_as_edge_transition_contract() {
        let (_, worktree_id, session_id) = ids();

        // Rising edge for a worktree-scoped session: carries session + worktree
        // ids and the boolean transition, never a timestamp.
        let rising = Event::OutputActive {
            session_id,
            worktree_id: Some(worktree_id),
            active: true,
        };
        let value: serde_json::Value = serde_json::to_value(&rising).unwrap();
        assert_eq!(value["type"], "output-active");
        assert_eq!(value["session_id"], session_id.to_string());
        assert_eq!(value["worktree_id"], worktree_id.to_string());
        assert_eq!(value["active"], true);
        // No timestamp field: the event is a pure edge transition.
        assert!(value.get("at").is_none());
        assert!(value.get("timestamp").is_none());
        let back: Event = serde_json::from_value(value).unwrap();
        assert_eq!(rising, back);

        // Falling edge for a project-root session: worktree_id is null.
        let falling = Event::OutputActive {
            session_id,
            worktree_id: None,
            active: false,
        };
        let value: serde_json::Value = serde_json::to_value(&falling).unwrap();
        assert_eq!(value["active"], false);
        assert!(value["worktree_id"].is_null());
        let back: Event = serde_json::from_value(value).unwrap();
        assert_eq!(falling, back);
    }

    #[test]
    fn catalog_mentions_core_message_families() {
        assert!(crate::MESSAGE_CATALOG.contains("Request:"));
        assert!(crate::MESSAGE_CATALOG.contains("Response:"));
        assert!(crate::MESSAGE_CATALOG.contains("Event:"));
        assert!(crate::MESSAGE_CATALOG.contains("session-output"));
        assert!(crate::MESSAGE_CATALOG.contains("output-active"));
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

    #[test]
    fn composite_job_messages_serialize_as_contract() {
        let (_, worktree_id, _) = ids();
        let job_id = JobId::new();

        // The composite chains start through the StartJob allowlist and map back
        // and forth across the bare-request boundary.
        let start = Request::StartJob {
            request: JobRequest::CommitAndPush {
                worktree_id,
                settings: None,
            },
        };
        let value: serde_json::Value = serde_json::to_value(&start).unwrap();
        assert_eq!(value["request"]["type"], "commit-and-push");
        let back: Request = serde_json::from_value(value).unwrap();
        assert_eq!(start, back);
        assert_eq!(
            JobRequest::try_from(Request::CommitAndPush {
                worktree_id,
                settings: None,
            })
            .unwrap(),
            JobRequest::CommitAndPush {
                worktree_id,
                settings: None,
            }
        );
        assert_eq!(
            JobRequest::try_from(Request::CreatePr {
                worktree_id,
                base: Some("main".into()),
                settings: None,
            })
            .unwrap(),
            JobRequest::CreatePr {
                worktree_id,
                base: Some("main".into()),
                settings: None,
            }
        );

        // Per-step progress identifies job, worktree, chain kind, step, and phase.
        let progress = Event::CompositeJobProgress {
            job_id,
            worktree_id,
            kind: CompositeJobKind::CommitAndPush,
            step: CompositeStep::Pushing,
            phase: StepPhase::Started,
        };
        let value: serde_json::Value = serde_json::to_value(&progress).unwrap();
        assert_eq!(value["type"], "composite-job-progress");
        assert_eq!(value["kind"], "commit-and-push");
        assert_eq!(value["step"], "pushing");
        assert_eq!(value["phase"], "started");
        assert_eq!(value["worktree_id"], worktree_id.to_string());
        let back: Event = serde_json::from_value(value).unwrap();
        assert_eq!(progress, back);

        // The success completion payload carries the toast's four fields.
        let done = Response::CommitAndPushed {
            result: CommitAndPushResult {
                subject: "feat: add proto".into(),
                short_sha: "abc1234".into(),
                pushed_commits: 1,
                file_count: 2,
            },
        };
        let value: serde_json::Value = serde_json::to_value(&done).unwrap();
        assert_eq!(value["type"], "commit-and-pushed");
        assert_eq!(value["result"]["short_sha"], "abc1234");
        assert_eq!(value["result"]["pushed_commits"], 1);
        assert_eq!(value["result"]["file_count"], 2);
        let back: Response = serde_json::from_value(value).unwrap();
        assert_eq!(done, back);

        // A push failure keeps the landed commit in `result.commit`.
        let failed = Response::CompositeJobFailed {
            kind: CompositeJobKind::CommitAndPush,
            failed_step: CompositeStep::Pushing,
            reason: "remote rejected push".into(),
            result: CompositeJobResult {
                commit: Some(CommitAndPushResult {
                    subject: "feat: add proto".into(),
                    short_sha: "abc1234".into(),
                    pushed_commits: 0,
                    file_count: 2,
                }),
            },
        };
        let value: serde_json::Value = serde_json::to_value(&failed).unwrap();
        assert_eq!(value["type"], "composite-job-failed");
        assert_eq!(value["failed_step"], "pushing");
        assert_eq!(value["result"]["commit"]["short_sha"], "abc1234");
        let back: Response = serde_json::from_value(value).unwrap();
        assert_eq!(failed, back);

        // The active-Jobs query and reply round-trip.
        let query = Request::ActiveJobs { worktree_id };
        let value: serde_json::Value = serde_json::to_value(&query).unwrap();
        assert_eq!(value["type"], "active-jobs");
        let back: Request = serde_json::from_value(value).unwrap();
        assert_eq!(query, back);

        let reply = Response::ActiveJobs {
            jobs: vec![ActiveJobInfo {
                job_id,
                worktree_id,
                kind: CompositeJobKind::CreatePr,
                step: CompositeStep::Drafting,
            }],
        };
        let value: serde_json::Value = serde_json::to_value(&reply).unwrap();
        assert_eq!(value["type"], "active-jobs");
        assert_eq!(value["jobs"][0]["kind"], "create-pr");
        assert_eq!(value["jobs"][0]["step"], "drafting");
        let back: Response = serde_json::from_value(value).unwrap();
        assert_eq!(reply, back);
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
                settings: Some(DraftGenerationSettings {
                    provider: DraftProvider::Codex,
                    model: None,
                    claude_path: None,
                    codex_path: Some(r"C:\Program Files\Codex\codex.exe".into()),
                    commit_instructions: None,
                    pr_instructions: None,
                }),
            },
            JobRequest::GenerateCommitDraft {
                worktree_id,
                settings: Some(DraftGenerationSettings {
                    provider: DraftProvider::Claude,
                    model: Some("sonnet".into()),
                    claude_path: Some(r"C:\Program Files\Claude\claude.exe".into()),
                    codex_path: None,
                    commit_instructions: None,
                    pr_instructions: None,
                }),
            },
            JobRequest::GeneratePullRequestDraft {
                worktree_id,
                base: Some("main".into()),
                settings: Some(DraftGenerationSettings {
                    provider: DraftProvider::Codex,
                    model: Some("gpt-5-codex".into()),
                    claude_path: None,
                    codex_path: Some(r"C:\Program Files\Codex\codex.exe".into()),
                    commit_instructions: None,
                    pr_instructions: None,
                }),
            },
            JobRequest::Push { worktree_id },
            JobRequest::Fetch { worktree_id },
            JobRequest::Pull { worktree_id },
            JobRequest::PrStatus { worktree_id },
            JobRequest::CreatePullRequest {
                worktree_id,
                title: "Add proto".into(),
                body: Some("Body".into()),
                base: Some("main".into()),
                draft: true,
            },
            JobRequest::CommitAndPush {
                worktree_id,
                settings: None,
            },
            JobRequest::CreatePr {
                worktree_id,
                base: Some("main".into()),
                settings: None,
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
            head_commit_id: Some("a".repeat(40)),
            base_branch: Some("main".into()),
            files: vec![ChangedFile {
                path: "src/lib.rs".into(),
                status: FileStatus::Modified,
                staged: false,
                additions: 12,
                deletions: 3,
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
            Request::ListDirectory {
                path: Some("/home/dev/code".into()),
                show_hidden: true,
            },
            Request::ListDirectory {
                path: None,
                show_hidden: false,
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
                staged: Some(false),
                ignore_whitespace: Some(true),
                context_lines: Some(10),
            },
            Request::GitLog {
                worktree_id,
                limit: 100,
                offset: 0,
            },
            Request::CommitDiff {
                worktree_id,
                commit_id: "a".repeat(40),
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
                settings: Some(DraftGenerationSettings {
                    provider: DraftProvider::Codex,
                    model: None,
                    claude_path: None,
                    codex_path: Some(r"C:\Program Files\Codex\codex.exe".into()),
                    commit_instructions: None,
                    pr_instructions: None,
                }),
            },
            Request::GenerateCommitDraft {
                worktree_id,
                settings: Some(DraftGenerationSettings {
                    provider: DraftProvider::Claude,
                    model: Some("sonnet".into()),
                    claude_path: Some(r"C:\Program Files\Claude\claude.exe".into()),
                    codex_path: None,
                    commit_instructions: None,
                    pr_instructions: None,
                }),
            },
            Request::GeneratePullRequestDraft {
                worktree_id,
                base: Some("main".into()),
                settings: Some(DraftGenerationSettings {
                    provider: DraftProvider::Codex,
                    model: Some("gpt-5-codex".into()),
                    claude_path: None,
                    codex_path: Some(r"C:\Program Files\Codex\codex.exe".into()),
                    commit_instructions: None,
                    pr_instructions: None,
                }),
            },
            Request::Fetch { worktree_id },
            Request::Push { worktree_id },
            Request::Pull { worktree_id },
            Request::CreatePullRequest {
                worktree_id,
                title: "Add proto".into(),
                body: Some("Body".into()),
                base: Some("main".into()),
                draft: true,
            },
            Request::CommitAndPush {
                worktree_id,
                settings: None,
            },
            Request::CreatePr {
                worktree_id,
                base: Some("main".into()),
                settings: None,
            },
            Request::ActiveJobs { worktree_id },
            Request::InstallAgentHooks { worktree_id },
            Request::ReportAgentState {
                agent: KnownAgent::ClaudeCode,
                state: Some(AgentState::NeedsApproval),
                session_id: Some(session_id),
                cwd: Some("/repo".into()),
                detail: Some("permission prompt".into()),
                agent_run_id: Some("claude-run-1".into()),
            },
            Request::AnnounceAgent {
                agent: KnownAgent::ClaudeCode,
                session_id: Some(session_id),
                cwd: Some("/repo".into()),
                agent_run_id: Some("claude-run-1".into()),
            },
            Request::Ping,
            Request::StartJob {
                request: JobRequest::Push { worktree_id },
            },
            Request::CancelJob {
                job_id: JobId::new(),
            },
            Request::BeginUpload {
                session_id,
                file_name: "report.pdf".into(),
                total_bytes: 1_048_576,
            },
            Request::UploadChunk {
                upload_id: "upload-1".into(),
                byte_count: 4096,
            },
            Request::FinishUpload {
                upload_id: "upload-1".into(),
            },
            Request::AbortUpload {
                upload_id: "upload-1".into(),
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
                os_family: OsFamily::Unix,
                exe_path: Some("/home/dev/.local/bin/hitch".into()),
            },
            Response::UploadStarted {
                upload_id: "upload-1".into(),
                final_name: "file-1.txt".into(),
            },
            Response::UploadFinished {
                remote_path: "/home/dev/.hitch/uploads/abc/file-1.txt".into(),
            },
            Response::Ack,
            Response::Projects {
                projects: vec![project],
            },
            Response::DirectoryListing {
                path: "/home/dev".into(),
                parent: Some("/home".into()),
                home: "/home/dev".into(),
                entries: vec![DirEntry {
                    name: "code".into(),
                    path: "/home/dev/code".into(),
                }],
            },
            Response::Worktrees {
                worktrees: vec![worktree],
            },
            Response::Sessions {
                sessions: vec![session.clone()],
            },
            Response::SessionOpened {
                session,
                agent: Some(KnownAgent::ClaudeCode),
                agent_state: Some(AgentState::NeedsApproval),
                agent_detail: Some("permission prompt".into()),
                output_active: true,
            },
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
            Response::CommitLog {
                commits: vec![CommitInfo {
                    id: "a".repeat(40),
                    summary: Some("feat: add proto".into()),
                    body: Some("Body line".into()),
                    author: Some("Hitch Test".into()),
                    time: 1_700_000_000,
                    is_merge: false,
                    ahead_of_base: true,
                    additions: 12,
                    deletions: 3,
                }],
                has_more: true,
            },
            Response::CommitDiff {
                meta: CommitMeta {
                    id: "a".repeat(40),
                    summary: Some("feat: add proto".into()),
                    body: Some("Body line".into()),
                    author: Some("Hitch Test".into()),
                    time: 1_700_000_000,
                    is_merge: false,
                    additions: 12,
                    deletions: 3,
                },
                files: vec![CommitFileDiff {
                    path: "src/lib.rs".into(),
                    status: FileStatus::Modified,
                    diff: "diff --git a/src/lib.rs b/src/lib.rs".into(),
                }],
            },
            Response::PullRequestCreated {
                url: "https://github.com/example/hitch/pull/1".into(),
            },
            Response::CommitAndPushed {
                result: CommitAndPushResult {
                    subject: "feat: add proto".into(),
                    short_sha: "abc1234".into(),
                    pushed_commits: 1,
                    file_count: 2,
                },
            },
            Response::CompositeJobFailed {
                kind: CompositeJobKind::CommitAndPush,
                failed_step: CompositeStep::Pushing,
                reason: "remote rejected push".into(),
                result: CompositeJobResult {
                    commit: Some(CommitAndPushResult {
                        subject: "feat: add proto".into(),
                        short_sha: "abc1234".into(),
                        pushed_commits: 0,
                        file_count: 2,
                    }),
                },
            },
            Response::ActiveJobs {
                jobs: vec![ActiveJobInfo {
                    job_id: JobId::new(),
                    worktree_id,
                    kind: CompositeJobKind::CreatePr,
                    step: CompositeStep::Drafting,
                }],
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
            Event::SessionOpened {
                session,
                agent: Some(KnownAgent::ClaudeCode),
                agent_state: Some(AgentState::Running),
                agent_detail: Some("working".into()),
                output_active: true,
            },
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
                agent: Some(KnownAgent::Codex),
                state: Some(AgentState::Running),
                detail: None,
            },
            Event::OutputActive {
                session_id,
                worktree_id: Some(worktree_id),
                active: true,
            },
            Event::OutputActive {
                session_id,
                worktree_id: None,
                active: false,
            },
            Event::WorktreeDirty {
                worktree_id,
                dirty: true,
            },
            Event::WorktreeUpdated { worktree },
            Event::WorktreeRemoved { worktree_id },
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
            Event::CompositeJobProgress {
                job_id: JobId::new(),
                worktree_id,
                kind: CompositeJobKind::CommitAndPush,
                step: CompositeStep::Committing,
                phase: StepPhase::Started,
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
            ControlMessage::ConnEnv {
                ssh_auth_sock: "/tmp/agent.sock".into(),
            },
            ControlMessage::SshAgentRelay,
            ControlMessage::SshAgentOpen { channel: 7 },
            ControlMessage::ssh_agent_data(7, b"\x00\x01\x02ssh-agent bytes\xff"),
            ControlMessage::SshAgentClose { channel: 7 },
            ControlMessage::ClientActive,
        ]
    }

    #[test]
    fn ssh_agent_data_round_trips_through_base64() {
        let raw: Vec<u8> = (0u16..=511).map(|b| b as u8).collect();
        let message = ControlMessage::ssh_agent_data(3, &raw);
        let ControlMessage::SshAgentData { channel, data } = &message else {
            panic!("expected SshAgentData");
        };
        assert_eq!(*channel, 3);
        assert_eq!(decode_ssh_agent_data(data).unwrap(), raw);
        // Survives the JSON control-line encode the proxy bridges verbatim.
        let json = serde_json::to_string(&message).unwrap();
        let back: ControlMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(message, back);
    }

    #[test]
    fn ssh_agent_relay_serializes_as_unit_kind() {
        let json = serde_json::to_string(&ControlMessage::SshAgentRelay).unwrap();
        assert_eq!(json, r#"{"kind":"ssh-agent-relay"}"#);
        let back: ControlMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ControlMessage::SshAgentRelay);
    }

    #[test]
    fn decode_ssh_agent_data_enforces_size_cap() {
        // At the cap: accepted.
        let at_cap = vec![0u8; SSH_AGENT_RELAY_MAX_FRAME];
        let encoded = encode_ssh_agent_data(&at_cap);
        assert_eq!(decode_ssh_agent_data(&encoded).unwrap().len(), at_cap.len());

        // One byte over the cap: rejected as TooLarge, not silently truncated.
        let over_cap = vec![0u8; SSH_AGENT_RELAY_MAX_FRAME + 1];
        let encoded = encode_ssh_agent_data(&over_cap);
        assert_eq!(
            decode_ssh_agent_data(&encoded),
            Err(SshAgentDataError::TooLarge)
        );

        // Garbage base64 is rejected, never panics.
        assert_eq!(
            decode_ssh_agent_data("not valid base64 !!!"),
            Err(SshAgentDataError::InvalidBase64)
        );
    }
}
