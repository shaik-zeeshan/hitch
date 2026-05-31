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

export type AgentState = "running" | "needs-approval" | "completed" | "error";

// Daemon Status — the daemon process's own liveness, distinct from this window's
// socket link (CONTEXT.md, ADR 0009). Mirrors src-tauri's `DaemonStatus`.
export type DaemonStatus = "starting" | "running" | "unreachable" | "failed";

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
};

export type GitStatus = {
  worktree_id: Id;
  branch: string;
  dirty: boolean;
  ahead: number;
  behind: number;
  additions: number;
  deletions: number;
  files: ChangedFile[];
};

export type FileDiff = {
  worktree_id: Id;
  path: string;
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

export type Request = { type: string; [key: string]: unknown };

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
  | { type: "list-draft-models"; provider: string }
  | {
      type: "generate-commit-draft";
      worktree_id: Id;
      settings: { provider: string; model: string | null } | null;
    }
  | {
      type: "generate-pull-request-draft";
      worktree_id: Id;
      base: string | null;
      settings: { provider: string; model: string | null } | null;
    }
  | { type: "push"; worktree_id: Id }
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
    };

export type StartJobRequest = { type: "start-job"; request: JobRequest };
export type Response = { type: string; [key: string]: unknown };
export type HitchEvent = { type: string; [key: string]: unknown };

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
// blindness. `cls` matches the .pill.{run,approval,done,error} classes. The
// labels are phrased for a person glancing at a branch they're NOT in: "what is
// that agent doing?" — working / awaiting input / completed.
export const AGENT_LABEL: Record<AgentState, { label: string; cls: string }> = {
  running: { label: "working", cls: "run" },
  "needs-approval": { label: "awaiting input", cls: "approval" },
  completed: { label: "completed", cls: "done" },
  error: { label: "error", cls: "error" },
};

// When a worktree (or project) holds several agent sessions, the row shows the
// one that most needs attention: a blocked approval outranks an error, which
// outranks an in-progress run, which outranks a finished one.
export const AGENT_PRIORITY: AgentState[] = [
  "needs-approval",
  "error",
  "running",
  "completed",
];

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
