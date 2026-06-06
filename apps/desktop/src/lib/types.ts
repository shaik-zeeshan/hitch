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
