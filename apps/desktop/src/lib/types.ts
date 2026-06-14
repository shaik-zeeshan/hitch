// Domain types — ported verbatim from the provisional React client (App.tsx).
// These mirror the daemon's wire contract; the daemon is the source of truth and
// the names/shapes here must not drift from it. See docs/adr/0006-frontend-stack.md.

export type Id = string;
export type ProjectKind = "git-backed" | "plain";

export type Project = {
  id: Id;
  name: string;
  root: string;
  kind: ProjectKind;
};

export type Worktree = {
  id: Id;
  project_id: Id;
  path: string;
  branch: string;
  is_main: boolean;
  is_hitch_managed: boolean;
};

export type SessionParent =
  | { kind: "worktree"; id: Id }
  | { kind: "project"; id: Id };

export type Session = {
  id: Id;
  name: string;
  parent: SessionParent;
  cwd: string;
};

export type AgentState = "running" | "needs-approval" | "waiting" | "error";
export type KnownAgent = "claude-code" | "codex";


// Daemon Status — the daemon process's own liveness, distinct from this window's
// socket link (CONTEXT.md, ADR 0009). Mirrors src-tauri's `DaemonStatus`.
export type DaemonStatus = "starting" | "running" | "unreachable" | "failed";

// ---- Daemon scope (ADR 0014, issue #25) -----------------------------------
//
// A GUI window may attach to several Daemons at once: the local Daemon plus
// zero or more remote Daemons reached through SSH Hosts (CONTEXT.md). Every
// Project, Worktree, Session, and Job identifier is interpreted within its
// OWNING daemon scope, never as globally unique across attached daemons (ADR
// 0014). A `DaemonScopeId` names one such scope; the local Daemon is the
// well-known `LOCAL_SCOPE_ID`. Saved SSH Hosts will mint their own scope ids in
// issue #27 — this slice ships Local only.
export type DaemonScopeId = string;

// The well-known scope id of the local Daemon. Stable across reloads so a
// persisted Local expand/collapse state and any scope-keyed selection survive
// a restart. SSH Host scope ids (issue #27) are minted per saved host and will
// never collide with this reserved value.
export const LOCAL_SCOPE_ID: DaemonScopeId = "local";

// Whether a scope is the local Daemon or a remote one reached through an SSH
// Host. Only `local` exists today; `ssh-host` is reserved for issue #27 so the
// tree, status surfaces, and selection model already branch on the kind.
export type DaemonScopeKind = "local" | "ssh-host";

// One attached Daemon presented as a top-level scope in the multi-daemon tree
// (ADR 0014): Local first, saved SSH Hosts sorted alphabetically by target.
// `label` is the row's mono caption (`LOCAL`, or an SSH target). `status` is the
// scope's own Daemon Status — broader than this window's socket link — so a
// collapsed scope can still show liveness. SSH-only fields (target, connection
// backoff) are deliberately omitted here and added with the SSH Host model.
export type DaemonScope = {
  id: DaemonScopeId;
  kind: DaemonScopeKind;
  label: string;
  status: DaemonStatus;
};

// ---- SSH Host (ADR 0014, issue #26) ---------------------------------------
//
// A GUI-local saved OpenSSH target string through which the GUI can reach a
// Hitch Daemon running on that host (CONTEXT.md). It stores ONLY the target
// string — no private keys, passphrases, ports, or usernames as separate
// fields: OpenSSH config, ssh-agent, hardware keys, ProxyJump, and known_hosts
// remain the source of truth (ADR 0014). `id` is the well-known scope id this
// host mints (`ssh:<target>`), so issue #27 can interpret remote entities under
// a stable per-host scope without a separate rename-prone identity.
export type SshHost = {
  id: DaemonScopeId;
  target: string;
  // Forward the local ssh-agent on the proxy ssh so the persistent remote daemon
  // can sign git push/pull/fetch through it, with no prompt on the remote
  // (silly-ridge-27). Defaults on; legacy hosts lacking the field are read as on.
  forwardAgent?: boolean;
};

// The actionable failure categories the backend classifier returns for a failed
// Test Connection (ADR 0014). Mirrors src-tauri's `FailureCategory` (kebab-case).
export type SshTestCategory =
  | "auth"
  | "host-key"
  | "missing-hitch"
  | "protocol-mismatch"
  | "proxy-startup"
  | "network";

// Structured result of `test_ssh_host`. Mirrors src-tauri's `SshTestResult`:
// `ok` true means the Hello handshake succeeded at a compatible protocol
// version; otherwise `category` + a user-facing `message` (which embeds the
// exact manual `ssh … hitch daemon proxy` command) and an optional `detail`
// (stderr tail or version numbers).
export type SshTestResult = {
  ok: boolean;
  category?: SshTestCategory;
  message: string;
  detail?: string;
};

// Status of the local `hitch` CLI install (ADR 0014 amendment). Mirrors
// src-tauri's `cli_install::CliInstallStatus`. `state` drives the Remote Hosts
// "this machine as a remote host" control:
//  - `installed`     — our `~/.local/bin/hitch` symlink is present and ours,
//  - `not-installed` — nothing at the link path; Install can proceed,
//  - `conflict`      — a foreign file occupies the link path; we won't clobber,
//  - `unavailable`   — no bundled daemon (dev build) or Windows (manual install).
// The client reaches a self-installed host by learning the daemon's absolute path
// from the Hello handshake (ADR 0014 amendment 2026-06-08), so install no longer
// edits shell rc files and there is no PATH signal to report.
export type CliInstallState = "installed" | "not-installed" | "conflict" | "unavailable";
export type CliInstallStatus = {
  state: CliInstallState;
  linkPath: string | null;
  target: string | null;
  detail: string | null;
};

// Lifecycle of an async Job (CONTEXT.md, ADR 0008). Mirrors hitch-proto's
// `JobStatus`.
export type JobStatus =
  | "queued"
  | "running"
  | "succeeded"
  | "failed"
  | "cancelled";

export type FileStatus =
  | "added"
  | "modified"
  | "deleted"
  | "renamed"
  | "copied"
  | "untracked"
  | "conflicted";

export type ChangedFile = {
  path: string;
  status: FileStatus;
  staged: boolean;
  // Added/deleted line counts for the side this row represents (staged counts
  // when staged, worktree counts otherwise). Optional for rolling upgrades —
  // an older daemon omits them; the UI treats absent as 0.
  additions?: number;
  deletions?: number;
};

export type GitStatus = {
  worktree_id: Id;
  branch: string;
  dirty: boolean;
  ahead: number;
  behind: number;
  additions: number;
  deletions: number;
  // Full SHA of the current HEAD commit, or `null` on an unborn HEAD. The
  // History view refetches its log when this changes. Optional for rolling
  // upgrades — an older daemon omits it (serde `default`).
  head_commit_id?: string | null;
  // The daemon-resolved base branch (the project's main-worktree branch, falling
  // back to the repo's default). The single definition of the base convention,
  // consumed by `defaultBase`. Optional for rolling upgrades — an older daemon
  // omits it (serde `default`); `null` also means "no cross-branch base" (the
  // main worktree relative to itself).
  base_branch?: string | null;
  files: ChangedFile[];
};

export type FileDiff = {
  worktree_id: Id;
  path: string;
  diff: string;
};

// One commit row in a History `CommitLog` page. Mirrors hitch-proto's
// `CommitInfo` (struct fields are snake_case in the wire JSON). `time` is unix
// seconds; `summary`/`body`/`author` are null when absent.
export type CommitInfo = {
  id: string;
  summary: string | null;
  body: string | null;
  author: string | null;
  time: number;
  is_merge: boolean;
  ahead_of_base: boolean;
  additions: number;
  deletions: number;
};

// The metadata header of a Commit Tab. Mirrors hitch-proto's `CommitMeta`,
// which is `CommitInfo` minus `ahead_of_base`.
export type CommitMeta = Omit<CommitInfo, "ahead_of_base">;

// One file's diff within a Commit Tab. Mirrors the working-tree per-file diff
// shape (path + status + patch text) so the frontend reuses its diff renderer.
export type CommitFileDiff = {
  path: string;
  status: FileStatus;
  diff: string;
};

// A GitHub PR for a worktree's current branch, as returned by `gh pr view`.
// GitHub may return an open, closed, or merged PR for the branch; only an open
// PR is terminal for the desktop's Create-PR action.
export type PrInfo = {
  number: number;
  url: string;
  state: "OPEN" | "CLOSED" | "MERGED";
  draft: boolean;
};
export function isOpenPr(pr: PrInfo | null | undefined): pr is PrInfo {
  return pr?.state === "OPEN";
}

export type PrFields = {
  title: string;
  body: string | null;
  base: string | null;
  draft: boolean;
};

export type CommitDraft = {
  subject: string;
  body: string;
};

export type PullRequestDraft = {
  title: string;
  body: string;
};

export type BranchSummary = {
  name: string;
  is_remote: boolean;
};

// ---- composite Jobs (ADR 0013 amendment 2026-06-07) -----------------------
//
// The two daemon-owned autonomous chains. Wire tags mirror hitch-proto's
// `CompositeJobKind` / `CompositeStep` / `StepPhase` (kebab-case).

// Which chain a progress event / active-Job entry belongs to. Same strings the
// UI-facing job kind carries, so `Job.kind` and these line up.
export type CompositeJobKind = "commit-and-push" | "create-pr";

// One rung of a chain. `commit-and-push` runs staging → drafting → committing →
// pushing; `create-pr` runs pushing → drafting → creating-pr.
export type CompositeStep =
  | "staging"
  | "drafting"
  | "committing"
  | "pushing"
  | "creating-pr";

// Whether a step is starting or has finished (a step is the "current" rung
// between its started and finished phases).
export type StepPhase = "started" | "finished";

// Terminal payload of a successful `commit-and-push` chain (rides inside the
// `job-completed` envelope as `commit-and-pushed`). The auto-mode toast reads all
// four fields: subject · short sha · pushed count · file count.
export type CommitAndPushResult = {
  subject: string;
  short_sha: string;
  pushed_commits: number;
  file_count: number;
};

// Whatever a chain completed before failing (e.g. the commit that landed before
// a push failure). Absent when it aborted before producing anything (a
// draft-generation failure aborts before any commit).
export type CompositeJobResult = {
  commit?: CommitAndPushResult | null;
};

// One in-flight chain for a worktree, returned by the `active-jobs` query so a
// re-attaching GUI restores the exact button/Composer step.
export type ActiveJobInfo = {
  job_id: Id;
  worktree_id: Id;
  kind: CompositeJobKind;
  step: CompositeStep;
};

// One child directory in a remote-folder-browser listing (ADR 0014). The browser
// is folders-first and only folders are selectable, so the daemon lists folders
// only. `path` is the absolute path so the GUI navigates/AddProjects it without
// re-joining (the GUI never maps remote paths onto local paths).
export type DirEntry = {
  name: string;
  path: string;
};

// A directory listing returned by `list-directory` for the remote folder browser.
// `parent` is null at the filesystem root; `home` backs the browser's Home control.
export type DirectoryListing = {
  path: string;
  parent: string | null;
  home: string;
  entries: DirEntry[];
};

export type Request = { type: string; [key: string]: unknown };
export type GitDiffRequest = {
  type: "git-diff";
  worktree_id: Id;
  path: string;
  // Optional for protocol compatibility. Omitted requests keep daemon legacy
  // worktree-first, staged-fallback selection.
  staged?: boolean;
  // Diff-shaping options, serialized snake_case like the rest of the request.
  // Both are serde skip-if-none on the daemon, so the frontend omits them at
  // their defaults (no whitespace flag, 3 context lines) to keep older daemons
  // happy. `ignore_whitespace` true asks git to ignore whitespace-only changes;
  // `context_lines` overrides git's default of 3 lines of surrounding context.
  ignore_whitespace?: boolean;
  context_lines?: number;
};

// Read a page of the worktree's enriched HEAD commit log (History view). A fast
// synchronous git read, not a Job. Replies with a `commit-log` response.
export type GitLogRequest = {
  type: "git-log";
  worktree_id: Id;
  limit: number;
  offset: number;
};

// Read one commit's metadata plus per-file first-parent diff in one round-trip
// (Commit Tab). A fast synchronous git read; replies with `commit-diff`.
export type CommitDiffRequest = {
  type: "commit-diff";
  worktree_id: Id;
  commit_id: string;
};


// Force a fresh full repaint of a session's PTY child after its size has
// settled (daemon replies with an Ack). Modeled on the inline `resize-session`
// request shape (`{ type, session_id }`) used in daemon.ts; the daemon is the
// source of truth for this contract. `Request` is structurally open, but we
// name this variant so the frontend emits exactly the agreed shape.
export type RepaintSessionRequest = { type: "repaint-session"; session_id: Id };

export type DraftGenerationSettings = {
  provider: string;
  model: string | null;
  claude_path: string | null;
  codex_path: string | null;
  // Draft Instructions appended to the built-in draft prompts as an extra
  // block; never replace the prompt or its JSON output contract (ADR 0007
  // amendment 2026-06-07). Optional on the wire (`#[serde(default)]` daemon
  // side), so existing call sites that omit them still compile.
  commit_instructions?: string | null;
  pr_instructions?: string | null;
};

// Shared allowlist accepted inside `start-job`. Keep this in lockstep with
// hitch-proto's `JobRequest` so the desktop cannot advertise unsupported work.
export type JobRequest =
  | {
      type: "clone-project";
      remote_url: string;
      destination: string;
      name: string | null;
    }
  | {
      type: "create-worktree";
      project_id: Id;
      branch: string;
      base: string | null;
      mode: "new-branch" | "existing-branch";
    }
  | {
      type: "list-draft-models";
      provider: string;
      settings: DraftGenerationSettings | null;
    }
  | {
      type: "generate-commit-draft";
      worktree_id: Id;
      settings: DraftGenerationSettings | null;
    }
  | {
      type: "generate-pull-request-draft";
      worktree_id: Id;
      base: string | null;
      settings: DraftGenerationSettings | null;
    }
  | { type: "push"; worktree_id: Id }
  | { type: "fetch"; worktree_id: Id }
  | { type: "pull"; worktree_id: Id }
  | { type: "pr-status"; worktree_id: Id }
  | { type: "project-pr-statuses"; project_id: Id }
  | {
      type: "create-pull-request";
      worktree_id: Id;
      title: string;
      body: string | null;
      base: string | null;
      draft: boolean;
    }
  // The two daemon-owned composite chains (ADR 0013 amendment 2026-06-07). Each
  // runs as ONE Job whose steps are reported as `composite-job-progress` events;
  // completion rides the existing `job-completed` envelope. `commit-and-push`
  // carries the COMMIT draft instructions; `create-pr` carries the PR ones.
  | {
      type: "commit-and-push";
      worktree_id: Id;
      settings: DraftGenerationSettings | null;
    }
  | {
      type: "create-pr";
      worktree_id: Id;
      base: string | null;
      settings: DraftGenerationSettings | null;
    };

export type StartJobRequest = { type: "start-job"; request: JobRequest };
export type Response = { type: string; [key: string]: unknown };
export type WorktreeRemovedEvent = { type: "worktree-removed"; worktree_id: Id };
export type HitchEvent = WorktreeRemovedEvent | { type: string; [key: string]: unknown };

// ---- display maps ---------------------------------------------------------

// Single-letter badge for a changed file's status (right-rail Changes list).
export const STATUS_GLYPH: Record<FileStatus, string> = {
  added: "A",
  modified: "M",
  deleted: "D",
  renamed: "R",
  copied: "C",
  untracked: "U",
  conflicted: "!",
};

// The status badge color class is keyed off the leading glyph in the mockup
// (.frow .st.M / .A / .D / .U); anything else falls back to the neutral U tint.
export function statusGlyphClass(status: FileStatus): string {
  const glyph = STATUS_GLYPH[status];
  return glyph === "M" || glyph === "A" || glyph === "D" ? glyph : "U";
}

// Agent state renders as a human-language WORD inside a tinted pill, in a
// reserved hue (never a bare symbol), so it survives grayscale and color
// blindness. `cls` matches the .pill.{run,approval,wait,error} classes. The
// labels are phrased for a person glancing at a branch they're NOT in: "what is
// that agent doing?" — working / awaiting input.
//
// `waiting` stays in the taxonomy (it is hook-truth that a turn ended — it drops
// `running` and floors the rollup) but renders UNLABELED (ADR 0011 amendment
// 2026-06-05): a finished turn is visually just idle, so there is no entry here.
// Only the act states (`needs-approval`, `error`) and the live `running` word
// are ever shown.
export const AGENT_LABEL: Partial<Record<AgentState, { label: string; cls: string }>> = {
  running: { label: "working", cls: "run" },
  "needs-approval": { label: "awaiting input", cls: "approval" },
  error: { label: "error", cls: "error" },
};

// The ACT states (ADR 0011 amendment / CONTEXT.md): the states that demand the
// user's attention. One predicate drives every attention surface — the row's
// state word, the tab needdot, and the collapsed-project rollup pill.
export const ACT_STATES = ["needs-approval", "error"] as const;
export type ActState = (typeof ACT_STATES)[number];

// `state ∈ {needs-approval, error}` → needs action. The single source of truth
// for "does this session demand attention".
export function needsAction(state: AgentState | null | undefined): state is ActState {
  return state === "needs-approval" || state === "error";
}

// When a worktree (or project) holds several agent sessions, the row shows the
// one that most needs attention. CONTEXT.md fixes the priority order:
// `needs-approval > error > waiting > running` — a blocked approval outranks a
// failed turn, which outranks a turn idling on the user, which outranks a run.
export const AGENT_PRIORITY: AgentState[] = [
  "needs-approval",
  "error",
  "waiting",
  "running",
];

// Highest-priority act state among `states`, or null if none needs action.
// Mixed act states collapse to the single highest-priority one (the pill is
// always one word).
export function highestActState(
  states: Array<AgentState | undefined | null>,
): ActState | null {
  for (const candidate of AGENT_PRIORITY) {
    if (needsAction(candidate) && states.includes(candidate)) return candidate;
  }
  return null;
}

// A per-project (or per-worktree) act-state rollup: the highest-priority act
// state across the row's sessions plus the count of sessions currently in an
// act state. `null` means nothing in the row needs action.
export type ActRollup = { state: ActState; count: number };

export function aggregateActRollup(
  states: Array<AgentState | undefined | null>,
): ActRollup | null {
  const state = highestActState(states);
  if (!state) return null;
  const count = states.reduce((n, s) => (needsAction(s) ? n + 1 : n), 0);
  return { state, count };
}

export function aggregateAgentState(
  states: Array<AgentState | undefined>,
): AgentState | null {
  for (const candidate of AGENT_PRIORITY) {
    if (states.includes(candidate)) return candidate;
  }
  return null;
}

export function parentKey(parent: SessionParent): string {
  return `${parent.kind}:${parent.id}`;
}

export function sessionBelongsTo(
  session: Session,
  target: SessionParent | null,
): boolean {
  return target !== null && parentKey(session.parent) === parentKey(target);
}
