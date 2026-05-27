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
  files: ChangedFile[];
};

export type FileDiff = {
  worktree_id: Id;
  path: string;
  diff: string;
};

export type PrFields = {
  title: string;
  body: string | null;
  base: string | null;
  draft: boolean;
};

export type Request = { type: string; [key: string]: unknown };
export type Response = { type: string; [key: string]: unknown };
export type HitchEvent = { type: string; [key: string]: unknown };

export type SessionOutputPayload = {
  session_id: Id;
  data: string;
};

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

// Agent state is rendered as a WORD in a reserved hue (never a bare symbol), so
// it survives grayscale and color blindness. `cls` matches the mockup's
// .status.{run,approval,done,error} classes.
export const AGENT_LABEL: Record<AgentState, { label: string; cls: string }> = {
  running: { label: "running", cls: "run" },
  "needs-approval": { label: "needs approval", cls: "approval" },
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
