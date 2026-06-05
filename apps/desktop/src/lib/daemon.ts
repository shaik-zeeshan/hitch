// Data layer — the single owner of the daemon connection and the reactive app
// state behind the shell. Ported from the provisional React client (App.tsx);
// the request `type` strings, Tauri command names, and derived rollups are the
// contract — copied here, not redesigned. See docs/adr/0006-frontend-stack.md.
//
// State lives in Svelte stores so any component can read it with `$store` and
// the cross-cutting fix-up logic (selection fallbacks, stale-state cleanup)
// runs once here as store subscriptions rather than per-component effects.

import { Channel, invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import { derived, get, writable } from "svelte/store";
import { ByteRing } from "./byteRing";
import {
  aggregateActRollup,
  aggregateAgentState,
  parentKey,
  sessionBelongsTo,
  type ActRollup,
  type AgentState,
  type BranchSummary,
  type ChangedFile,
  type CommitDraft,
  type DaemonStatus,
  type DraftGenerationSettings,
  type FileStatus,
  type GitStatus,
  type HitchEvent,
  type Id,
  type JobStatus,
  type JobRequest,
  type KnownAgent,
  type PrFields,
  type PrInfo,
  isOpenPr,
  type PullRequestDraft,
  type Project,
  type RepaintSessionRequest,
  type Request,
  type Response,
  type StartJobRequest,
  type Session,
  type SessionParent,
  type Worktree,
} from "./types";
import {
  draftClaudePath,
  draftCodexPath,
  draftModel,
  draftProvider,
  type DraftProvider,
} from "./settings";

export type Connection = "connecting" | "ready" | "offline";

// ---- base stores ----------------------------------------------------------

export const connection = writable<Connection>("connecting");
export const error = writable<string | null>(null);

// ---- Daemon Status (ADR 0009) ---------------------------------------------
//
// The four-state liveness of the daemon process itself, pushed from src-tauri
// over `hitch-status`. `connection` (above) is the narrower per-window socket
// link, derived from this for the git-poll guard and the existing banner.
export const daemonStatus = writable<DaemonStatus>("starting");
export const daemonReason = writable<string | null>(null);
export const daemonLogPath = writable<string | null>(null);

// ---- Jobs (ADR 0008) ------------------------------------------------------
//
// Long-running daemon ops (clone/worktree creation, push/pull, PR, drafts)
// run as Jobs: the desktop sends `StartJob`, gets `JobStarted { job_id }`, and
// result arrives later in a `JobCompleted` event. This store mirrors live Jobs
// by id for quiet progress; `runJob` (below) bridges the event flow back to a
// Promise so callers keep their async API.
export type Job = {
  id: Id;
  status: JobStatus;
  message: string | null;
  kind: string | null;
  worktreeId: Id | null;
};
export const jobs = writable<Record<Id, Job>>({});

export const projects = writable<Project[]>([]);
export const worktrees = writable<Worktree[]>([]);
export const sessions = writable<Session[]>([]);

// Per-session hook-reported Agent State, the daemon-owned stored truth (ADR
// 0011). `waiting` lives here but renders unlabeled; `running` is gated to the
// WORKING display word by output activity (see `displaySessionStates`).
export const agentStates = writable<Record<Id, AgentState>>({});

// Per-session announced Agent identity (ADR 0011 amendment 2026-06-05): the
// agent's own `SessionStart` hook announces *which* agent the moment its TUI
// starts, replayed on attach via `SessionOpened`. This is the ONLY source for
// the Session mark's facepile/glyph — identity is never inferred from the
// session title or launch command. Cleared when the agent exits to `None`.
export const sessionAgents = writable<Record<Id, KnownAgent>>({});

// Per-session PTY output-activity gate (ADR 0011 amendment 2026-06-05): the
// daemon broadcasts edge-triggered `output-active` transitions (rising/falling)
// and seeds the current value in `SessionOpened`. `WORKING = running ∧
// output_active`; an interrupted/hung agent stops producing output, the gate
// falls, and the WORKING word drops to idle without the hook state changing.
export const sessionOutputActive = writable<Record<Id, boolean>>({});

// Per-session DISPLAY state the shell consumes (tabs, rollups). It applies two
// derivations to the stored hook state, both from the 2026-06-05 grill:
//  - `running` is gated by output activity: a session reporting `running` whose
//    output gate is closed renders as idle (the WORKING word is suppressed).
//  - `waiting` renders unlabeled, so it is omitted entirely.
// Act states (`needs-approval`, `error`) always pass through unchanged.
export const displaySessionStates = derived(
  [agentStates, sessionOutputActive],
  ([$agentStates, $outputActive]) => {
    const map: Record<Id, AgentState> = {};
    for (const [sessionId, state] of Object.entries($agentStates)) {
      if (state === "waiting") continue;
      if (state === "running" && !$outputActive[sessionId]) continue;
      map[sessionId] = state;
    }
    return map;
  },
);

// Live foreground command per session (the process the user is interacting
// with in the PTY), pushed by the daemon. Absent until the first report.
export const sessionCommands = writable<Record<Id, string | null>>({});
export const dirtyWorktrees = writable<Record<Id, boolean>>({});
export const worktreeLineStats = writable<Record<Id, { additions: number; deletions: number }>>({});

export const selectedProjectId = writable<Id | null>(null);
export const selectedWorktreeId = writable<Id | null>(null);
export const activeSessionId = writable<Id | null>(null);

// Git view state (consumed by the Changes panel + diff tab).
export const gitStatus = writable<GitStatus | null>(null);
export const diffPath = writable<string | null>(null);
export const diffText = writable<string | null>(null);
// Whether the diff tab is the active center view. The diff tab persists as a
// peer of the session tabs (so `diffPath` can be set while a terminal shows);
// this flag is what the tab bar and center pane switch on.
export const diffActive = writable<boolean>(false);
export const gitBusy = writable<boolean>(false);
export const prUrl = writable<string | null>(null);
// The PR (if any) GitHub has for the selected worktree's branch. `null` = none
// known (or not yet checked). This preserves closed/merged metadata for display;
// use `openPrInfo` when the action state machine needs an actually-open PR.
export const prInfo = writable<PrInfo | null>(null);
export const openPrInfo = derived(prInfo, ($pr) => (isOpenPr($pr) ? $pr : null));
// PR-per-worktree, so the sidebar can show a PR chip on each branch without each
// row firing its own lookup. Populated as a side effect of `loadPrStatus` (which
// already runs on worktree switch + after git ops), so a chip appears once a
// worktree's status has been fetched at least once. A missing key = not yet
// known (no chip); an explicit `null` = checked, no PR.
export const prByWorktree = writable<Record<Id, PrInfo | null>>({});

// Monotonic freshness clock shared by every path that writes `prByWorktree`
// (per-worktree `loadPrStatus` and the batched `loadProjectPrStatuses`). Each
// request stamps a seq at start; an entry is only overwritten by a seq at least
// as new as the one last applied to it, so a slow project-wide response can't
// regress a worktree whose per-worktree status was refreshed more recently
// (e.g. right after creating a PR).
let prByWorktreeSeq = 0;
const prByWorktreeApplied = new Map<Id, number>();
// Highest seq for which a lookup has *started* (not necessarily completed) per
// worktree. `prByWorktreeApplied` only advances on completed writes, so without
// this an older project-wide response that resolves while a newer per-worktree
// lookup is still in flight would apply stale data — and, if that fresher lookup
// then fails, leave the chip wrong until the next poll. Stamping the started seq
// lets the freshness guard reject any response older than an in-flight request.
const prByWorktreeStarted = new Map<Id, number>();
// The freshest seq known for a worktree, whether a lookup merely started or has
// fully landed. A write/clear is only fresh if its seq is >= this.
function freshestPrSeq(worktreeId: Id): number {
  return Math.max(
    prByWorktreeApplied.get(worktreeId) ?? 0,
    prByWorktreeStarted.get(worktreeId) ?? 0,
  );
}
// Returns whether this write was the freshest seen for `worktreeId` (i.e. it
// actually applied). Callers gate the selected worktree's single `prInfo` on the
// result so an unrelated project's batched lookup can't drop a newer per-worktree
// result through a shared global counter.
function writePrByWorktree(worktreeId: Id, pr: PrInfo | null, seq: number): boolean {
  if (seq < freshestPrSeq(worktreeId)) return false;
  prByWorktreeApplied.set(worktreeId, seq);
  prByWorktree.update((map) => ({ ...map, [worktreeId]: pr }));
  return true;
}

const diffCache = new Map<string, string>();
let diffRequestSeq = 0;
let statusRequestSeq = 0;
let statusPollTimer: ReturnType<typeof setInterval> | null = null;
let statusPollInFlight = false;
let prPollTimer: ReturnType<typeof setInterval> | null = null;
let prFocusHandler: (() => void) | null = null;

const STATUS_POLL_MS = 1_000;
// PR state changes on GitHub (e.g. draft → ready) are external, so poll for them
// while connected. `gh pr list` is network-priced and PR state rarely flips, and
// any change *we* cause is refreshed immediately by the post-git-op and focus
// paths — so the periodic poll only needs to catch external changes, where a few
// minutes of latency is fine. We also skip it while the window is hidden (the
// focus refresh catches up on return), so a backgrounded app spawns no `gh`.
const PR_POLL_MS = 180_000;

// ---- derived stores -------------------------------------------------------

export const selectedProject = derived(
  [projects, selectedProjectId],
  ([$projects, $id]) => $projects.find((p) => p.id === $id) ?? null,
);

export const projectWorktrees = derived(
  [worktrees, selectedProjectId],
  ([$worktrees, $id]) => $worktrees.filter((w) => w.project_id === $id),
);

// The terminal/diff "parent": a git-backed project drives off its selected
// worktree; a plain project is itself the parent (terminals only, no worktrees).
export const selectedParent = derived(
  [selectedProject, selectedWorktreeId],
  ([$project, $worktreeId]): SessionParent | null => {
    if ($worktreeId) return { kind: "worktree", id: $worktreeId };
    if ($project?.kind === "plain") return { kind: "project", id: $project.id };
    return null;
  },
);

export const gitWorktreeId = derived(selectedParent, ($parent) =>
  $parent?.kind === "worktree" ? $parent.id : null,
);

export const currentDirty = derived(
  [gitWorktreeId, dirtyWorktrees],
  ([$id, $dirty]) => ($id ? $dirty[$id] : undefined),
);

export const defaultBase = derived(projectWorktrees, ($worktrees) =>
  $worktrees.find((w) => w.is_main)?.branch ?? null,
);

export const visibleSessions = derived(
  [sessions, selectedParent],
  ([$sessions, $parent]) => $sessions.filter((s) => sessionBelongsTo(s, $parent)),
);

export const activeSession = derived(
  [sessions, activeSessionId],
  ([$sessions, $id]) => $sessions.find((s) => s.id === $id) ?? null,
);

// Per-worktree rollup of the DISPLAY state (act states always; `running` only
// while its output gate is open; `waiting` is unlabeled and never rolls up).
// The single running word on the worktree you are actively looking at is
// suppressed — you can see that agent live in the main pane.
export const agentStateByWorktree = derived(
  [worktrees, sessions, displaySessionStates, selectedWorktreeId, activeSessionId, diffActive],
  ([$worktrees, $sessions, $display, $selectedWorktreeId, $activeSessionId, $diffActive]) => {
    const activeSession = $sessions.find((session) => session.id === $activeSessionId);
    const activeRunningWorktree =
      !$diffActive &&
      activeSession?.parent.kind === "worktree" &&
      $display[activeSession.id] === "running"
        ? activeSession.parent.id
        : null;
    const map: Record<Id, AgentState> = {};
    for (const worktree of $worktrees) {
      const agg = aggregateAgentState(
        $sessions
          .filter((s) => s.parent.kind === "worktree" && s.parent.id === worktree.id)
          .map((s) => $display[s.id]),
      );
      if (!agg) continue;
      if (agg === "running" && worktree.id === $selectedWorktreeId && worktree.id === activeRunningWorktree) {
        continue;
      }
      map[worktree.id] = agg;
    }
    return map;
  },
);

// Per-project act-state rollup: the highest-priority act state across the
// project's sessions plus the count of sessions in an act state (ADR 0011
// amendment / CONTEXT.md). One pill, mixed act states collapse to the highest
// priority. `null` = nothing in the project needs action. The rollup counts the
// raw hook act states directly (act states are never output-gated), so a
// collapsed project never hides a session demanding attention.
export const agentActRollupByProject = derived(
  [projects, worktrees, sessions, agentStates],
  ([$projects, $worktrees, $sessions, $agentStates]) => {
    const map: Record<Id, ActRollup> = {};
    for (const project of $projects) {
      const projectWorktreeIds = new Set(
        $worktrees.filter((w) => w.project_id === project.id).map((w) => w.id),
      );
      const states = $sessions
        .filter(
          (s) =>
            (s.parent.kind === "worktree" && projectWorktreeIds.has(s.parent.id)) ||
            (s.parent.kind === "project" && s.parent.id === project.id),
        )
        .map((s) => $agentStates[s.id]);
      const rollup = aggregateActRollup(states);
      if (rollup) map[project.id] = rollup;
    }
    return map;
  },
);

// ---- request helper -------------------------------------------------------

function isError(response: Response): response is Response & {
  error: { message: string };
} {
  return response.type === "error";
}

export async function daemonRequest<T extends Response>(
  request: Request,
): Promise<T> {
  const response = await invoke<Response>("hitch_request", { request });
  if (isError(response)) {
    throw new Error(response.error.message);
  }
  return response as T;
}

function toMessage(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

// ---- Job dispatch (ADR 0008) ----------------------------------------------
//
// `runJob` wraps a job-capable request in `StartJob`, registers a pending
// resolver keyed by the returned job id, and resolves/rejects it when the
// matching `JobCompleted` event arrives (handled in the hitch-event listener).
// Callers (push/pull/draft fns) keep an ordinary Promise API.
type JobPending = {
  resolve: (response: Response) => void;
  reject: (error: Error) => void;
};
const jobPending = new Map<Id, JobPending>();
const EARLY_COMPLETION_TTL_MS = 30_000;
const EARLY_COMPLETION_MAX = 64;
type EarlyCompletion = { response: Response; receivedAt: number };
const earlyCompletions = new Map<Id, EarlyCompletion>();
const locallyStartedJobs = new Set<Id>();
let startingJobRequests = 0;
const cancellableJobKinds = new Set([
  "clone",
  "create-worktree",
  "push",
  "fetch",
  "pull",
  "create-pr",
  "draft-models",
  "commit-draft",
  "pr-draft",
]);

export function isJobCancellable(job: Job | null | undefined): boolean {
  return Boolean(job?.kind && cancellableJobKinds.has(job.kind));
}

function jobWorktreeId(request: JobRequest): Id | null {
  return "worktree_id" in request ? request.worktree_id : null;
}

export const cancellableJobForSelectedWorktree = derived(
  [jobs, gitWorktreeId],
  ([$jobs, $worktreeId]) => {
    if (!$worktreeId) return null;
    return (
      Object.values($jobs).find(
        (job) =>
          job.worktreeId === $worktreeId &&
          (job.status === "running" || job.status === "queued") &&
          isJobCancellable(job),
      ) ?? null
    );
  },
);

function pruneEarlyCompletions(now = Date.now()): void {
  for (const [jobId, completion] of earlyCompletions) {
    if (now - completion.receivedAt > EARLY_COMPLETION_TTL_MS) {
      earlyCompletions.delete(jobId);
    }
  }
  while (earlyCompletions.size > EARLY_COMPLETION_MAX) {
    const oldest = earlyCompletions.keys().next().value;
    if (oldest === undefined) break;
    earlyCompletions.delete(oldest);
  }
}

function rememberEarlyCompletion(jobId: Id, response: Response): void {
  earlyCompletions.set(jobId, { response, receivedAt: Date.now() });
  pruneEarlyCompletions();
}

function takeEarlyCompletion(jobId: Id): Response | null {
  pruneEarlyCompletions();
  const early = earlyCompletions.get(jobId);
  if (!early) return null;
  earlyCompletions.delete(jobId);
  return early.response;
}

export async function runJob<T extends Response>(
  request: JobRequest,
  kind: string | null = null,
): Promise<T> {
  startingJobRequests += 1;
  let started: Response & { job_id: Id };
  try {
    const startRequest: StartJobRequest = { type: "start-job", request };
    started = await daemonRequest<Response & { job_id: Id }>(startRequest);
  } finally {
    startingJobRequests = Math.max(0, startingJobRequests - 1);
  }
  const jobId = started.job_id;
  locallyStartedJobs.add(jobId);
  jobs.update((current) => ({
    ...current,
    [jobId]: { id: jobId, status: "running", message: null, kind, worktreeId: jobWorktreeId(request) },
  }));
  return new Promise<T>((resolve, reject) => {
    const early = takeEarlyCompletion(jobId);
    if (early) {
      locallyStartedJobs.delete(jobId);
      jobs.update((current) => {
        const next = { ...current };
        delete next[jobId];
        return next;
      });
      if (isError(early)) {
        reject(new Error(early.error.message));
      } else {
        resolve(early as T);
      }
      return;
    }
    jobPending.set(jobId, {
      resolve: (response) => resolve(response as T),
      reject,
    });
  });
}

// Update the live Jobs store from a `JobProgress` event. Exported as the seam
// the event listener and the unit tests both drive.
export function applyJobProgress(
  jobId: Id,
  status: JobStatus,
  message: string | null,
  kind: string | null = null,
): void {
  jobs.update((current) => ({
    ...current,
    [jobId]: {
      id: jobId,
      status,
      message,
      kind: kind ?? current[jobId]?.kind ?? null,
      worktreeId: current[jobId]?.worktreeId ?? null,
    },
  }));
}

// Resolve a Job from its `JobCompleted` event: the wrapped response rides inside.
export function completeJob(jobId: Id, response: Response): void {
  const pending = jobPending.get(jobId);
  const local = locallyStartedJobs.has(jobId);
  jobPending.delete(jobId);
  jobs.update((current) => {
    if (!(jobId in current)) return current;
    const next = { ...current };
    delete next[jobId];
    return next;
  });
  if (!pending) {
    if (local || startingJobRequests > 0) {
      rememberEarlyCompletion(jobId, response);
    }
    return;
  }
  locallyStartedJobs.delete(jobId);
  if (isError(response)) {
    pending.reject(new Error(response.error.message));
  } else {
    pending.resolve(response);
  }
}

// Reject every in-flight Job when the daemon link drops. Jobs are ephemeral and
// do NOT survive a daemon restart (CONTEXT.md): a Job that was running when the
// daemon stopped is reported failed so the user can re-trigger.
function failAllJobs(reason: string): void {
  const message = `daemon restarted: ${reason}`;
  for (const [, pending] of jobPending) {
    pending.reject(new Error(message));
  }
  jobPending.clear();
  earlyCompletions.clear();
  locallyStartedJobs.clear();
  jobs.set({});
}

// Ask the daemon to cancel a running Job (signals its worker to kill any child).
export async function cancelJob(jobId: Id): Promise<void> {
  try {
    await daemonRequest({ type: "cancel-job", job_id: jobId });
  } catch (err) {
    error.set(toMessage(err));
  }
}

function upsert<T extends { id: Id }>(items: T[], item: T): T[] {
  return items.some((existing) => existing.id === item.id)
    ? items.map((existing) => (existing.id === item.id ? item : existing))
    : [...items, item];
}

function omitKey<T>(record: Record<Id, T>, id: Id): Record<Id, T> {
  if (!(id in record)) return record;
  const next = { ...record };
  delete next[id];
  return next;
}

function removeWorktreeLocal(worktreeId: Id): void {
  const removedSessionIds = get(sessions)
    .filter((session) => session.parent.kind === "worktree" && session.parent.id === worktreeId)
    .map((session) => session.id);
  for (const sessionId of removedSessionIds) {
    closeSessionOutput(sessionId);
  }

  sessions.update((items) =>
    items.filter((session) => session.parent.kind !== "worktree" || session.parent.id !== worktreeId),
  );
  worktrees.update((items) => items.filter((worktree) => worktree.id !== worktreeId));
  dirtyWorktrees.update((current) => omitKey(current, worktreeId));
  worktreeLineStats.update((current) => omitKey(current, worktreeId));
  prByWorktree.update((current) => omitKey(current, worktreeId));
  prByWorktreeApplied.delete(worktreeId);
  prByWorktreeStarted.delete(worktreeId);
  const diffCachePrefix = `${worktreeId}\0`;
  for (const key of diffCache.keys()) {
    if (key.startsWith(diffCachePrefix)) diffCache.delete(key);
  }

  if (get(gitStatus)?.worktree_id === worktreeId) gitStatus.set(null);
  if (get(gitWorktreeId) === worktreeId) {
    selectedWorktreeId.set(null);
    prInfo.set(null);
    prUrl.set(null);
    closeDiff();
  }
}

// ---- snapshot / refresh ---------------------------------------------------

export async function refreshAll(
  options: { restoreLiveSessionSelection?: boolean } = {},
): Promise<void> {
  const projectResponse = await daemonRequest<Response & { projects: Project[] }>({
    type: "list-projects",
  });
  projects.set(projectResponse.projects);

  const worktreeLists = await Promise.all(
    projectResponse.projects
      .filter((project) => project.kind === "git-backed")
      .map(async (project) => {
        const response = await daemonRequest<Response & { worktrees: Worktree[] }>({
          type: "list-worktrees",
          project_id: project.id,
        });
        return response.worktrees;
      }),
  );
  const allWorktrees = worktreeLists.flat();
  worktrees.set(allWorktrees);

  // Populate every worktree's PR chip in the background — one batched lookup per
  // project, throttled internally. Not awaited: a slow `gh` must not hold up the
  // rest of the snapshot (sessions, dirty state).
  for (const project of projectResponse.projects) {
    if (project.kind === "git-backed") {
      void loadProjectPrStatuses(project.id);
    }
  }

  const sessionResponse = await daemonRequest<Response & { sessions: Session[] }>({
    type: "list-sessions",
    parent: null,
  });
  sessions.set(sessionResponse.sessions);
  if (options.restoreLiveSessionSelection) {
    restoreLiveSessionSelection(sessionResponse.sessions, allWorktrees);
  }
  reconcileSessionOutputs(sessionResponse.sessions);

  // Seed dirty indicators and line stats so the tree is useful before the Changes panel opens.
  const statusEntries = await Promise.all(
    allWorktrees.map(async (worktree) => {
      try {
        const response = await daemonRequest<Response & { status: GitStatus }>({
          type: "git-status",
          worktree_id: worktree.id,
        });
        return [worktree.id, response.status] as const;
      } catch {
        return [worktree.id, null] as const;
      }
    }),
  );
  dirtyWorktrees.set(
    Object.fromEntries(statusEntries.map(([id, status]) => [id, status?.dirty ?? false])),
  );
  worktreeLineStats.set(
    Object.fromEntries(
      statusEntries.map(([id, status]) => [
        id,
        { additions: status?.additions ?? 0, deletions: status?.deletions ?? 0 },
      ]),
    ),
  );
}

function restoreLiveSessionSelection(liveSessions: Session[], allWorktrees: Worktree[]): void {
  // The user already has a project/worktree selected — keep it. Whether or not
  // that selection currently has a live session, stay put rather than yanking
  // the user to an unrelated project/worktree. Only a fresh window with no
  // selection auto-jumps to the first live session below.
  if (get(selectedParent)) {
    return;
  }

  for (const session of liveSessions) {
    if (session.parent.kind === "project") {
      if (!get(projects).some((project) => project.id === session.parent.id)) continue;
      selectedProjectId.set(session.parent.id);
      selectedWorktreeId.set(null);
      activeSessionId.set(session.id);
      return;
    }

    const worktree = allWorktrees.find((item) => item.id === session.parent.id);
    if (!worktree) continue;
    selectedProjectId.set(worktree.project_id);
    selectedWorktreeId.set(worktree.id);
    activeSessionId.set(session.id);
    return;
  }
}
async function refreshSnapshotAfterConnect(): Promise<void> {
  try {
    await refreshAll({ restoreLiveSessionSelection: true });
  } catch (err) {
    error.set(toMessage(err));
  }
}


// ---- per-session PTY output (binary channel + bounded byte ring) ----------
//
// PTY bytes stream from the Tauri layer over a per-session binary Channel
// (ADR 0007); they are NEVER stringified in Rust, so xterm's own streaming
// UTF-8 decoder handles glyphs split across read boundaries. Here we hold each
// session's recent bytes in a bounded `ByteRing` (the GUI repaint copy; the
// daemon stays authoritative for scrollback) and fan new tails out to whichever
// Terminal component is currently mounted for that session.

type OutputSubscriber = {
  onReset: () => void;
  onData: (tail: Uint8Array) => void;
};

const rings = new Map<Id, ByteRing>();
const channels = new Map<Id, Channel<ArrayBuffer | Uint8Array>>();
// At most one Terminal is mounted per session at a time (the parent re-keys it),
// so a single subscriber slot per session is sufficient.
const subscribers = new Map<Id, OutputSubscriber>();

// The Channel delivers raw bytes as an ArrayBuffer (tauri 2's `Raw` IPC body);
// normalize whatever form arrives into a Uint8Array for the ring + xterm.
function toUint8Array(msg: ArrayBuffer | Uint8Array): Uint8Array {
  return msg instanceof Uint8Array ? msg : new Uint8Array(msg);
}

function ringFor(sessionId: Id): ByteRing {
  let ring = rings.get(sessionId);
  if (!ring) {
    ring = new ByteRing();
    rings.set(sessionId, ring);
  }
  return ring;
}

// Subscribe a mounted Terminal to a session's output. `onData` is called with
// the current ring contents immediately (catch-up) and again with each new tail;
// `onReset` fires when the ring is reset (a reconnect replay). Returns an
// unsubscribe.
export function subscribeSessionOutput(
  sessionId: Id,
  subscriber: OutputSubscriber,
): () => void {
  subscribers.set(sessionId, subscriber);
  // Catch up to whatever the ring already holds (e.g. output that arrived
  // before this terminal mounted, or a scrollback replay on reconnect).
  const ring = rings.get(sessionId);
  if (ring && ring.length > 0) subscriber.onData(ring.snapshot());
  return () => {
    if (subscribers.get(sessionId) === subscriber) subscribers.delete(sessionId);
  };
}

// Register (or re-register) a session's binary output channel with Tauri and
// reset its ring. Called on session-opened — both for brand-new sessions
// (empty ring, harmless) and on every reconnect, where the daemon replays the
// full scrollback as one SessionOutput; resetting here keeps that replay from
// duplicating the prior bytes.
function openSessionOutput(sessionId: Id): void {
  // Fresh ring so a reconnect replay repopulates from zero (no duplication).
  rings.set(sessionId, new ByteRing());
  subscribers.get(sessionId)?.onReset();

  let replayMode = true;
  const channel = new Channel<ArrayBuffer | Uint8Array>();
  channel.onmessage = (msg) => {
    const bytes = toUint8Array(msg);
    if (bytes.length === 0) return;
    const offsetBefore = ringFor(sessionId).totalSeen;

    // On reconnect, the daemon sends the full replay in one message, which can
    // exceed the ring capacity. Stream it directly to the subscriber while
    // retaining only the tail in the ring for future catch-up.
    if (replayMode) {
      // Send the full replay to the subscriber without truncation.
      subscribers.get(sessionId)?.onData(bytes);
      // Now add it to the ring, accepting that oversized replays will trim to
      // capacity; future live output will be bounded normally.
      ringFor(sessionId).append(bytes);
      replayMode = false;
      return;
    }

    // Normal live output path: bounded append and tail delivery.
    ringFor(sessionId).append(bytes);
    // Hand the subscriber exactly the new tail still retained by the ring.
    subscribers.get(sessionId)?.onData(ringFor(sessionId).bytesSince(offsetBefore));
  };
  channels.set(sessionId, channel);
  void Promise.resolve(invoke("register_session_output", { sessionId, channel })).catch(() => {
    if (channels.get(sessionId) === channel) channels.delete(sessionId);
  });
}

// Tear down a session's output: drop the ring + channel and tell Tauri to stop
// routing (and discard any staged bytes).
function closeSessionOutput(sessionId: Id): void {
  rings.delete(sessionId);
  channels.delete(sessionId);
  // A closing session must not have a trailing resize fire against its dead PTY.
  clearResizeDebounce(sessionId);
  void Promise.resolve(invoke("unregister_session_output", { sessionId })).catch(() => {});
}

function reconcileSessionOutputs(liveSessions: Session[]): void {
  const liveIds = new Set(liveSessions.map((session) => session.id));
  for (const sessionId of Array.from(channels.keys())) {
    if (!liveIds.has(sessionId)) closeSessionOutput(sessionId);
  }
  for (const session of liveSessions) {
    if (!channels.has(session.id)) openSessionOutput(session.id);
  }
}


// Set or clear a session's stored Agent State. `null` is the daemon's clear.
function applyAgentState(sessionId: Id, state: AgentState | null): void {
  if (state) {
    agentStates.update((current) =>
      current[sessionId] === state ? current : { ...current, [sessionId]: state },
    );
  } else {
    agentStates.update((current) => {
      if (!(sessionId in current)) return current;
      const next = { ...current };
      delete next[sessionId];
      return next;
    });
  }
}

// Set or clear a session's announced Agent identity (the Session mark source).
// `null` clears it back to the shell mark (agent exited to `None`).
function applySessionAgent(sessionId: Id, agent: KnownAgent | null): void {
  if (agent) {
    sessionAgents.update((current) =>
      current[sessionId] === agent ? current : { ...current, [sessionId]: agent },
    );
  } else {
    sessionAgents.update((current) => {
      if (!(sessionId in current)) return current;
      const next = { ...current };
      delete next[sessionId];
      return next;
    });
  }
}

export function applyHitchEvent(event: HitchEvent): void {
  if (event.type === "project-updated") {
    projects.update((items) => upsert(items, event.project as Project));
  }
  if (event.type === "worktree-updated") {
    worktrees.update((items) => upsert(items, event.worktree as Worktree));
  }
  if (event.type === "worktree-removed") {
    removeWorktreeLocal(event.worktree_id as Id);
  }
  if (event.type === "worktree-dirty") {
    const worktreeId = event.worktree_id as Id;
    const dirty = event.dirty as boolean;
    dirtyWorktrees.update((current) =>
      current[worktreeId] === dirty ? current : { ...current, [worktreeId]: dirty },
    );
    // Refresh the full status so the tree's line stats and, when selected,
    // the Changes panel track filesystem changes live.
    void loadGitStatus(worktreeId).catch(() => {});
  }
  if (event.type === "agent-state") {
    // The daemon broadcasts AgentState both for real state reports and for
    // identity announces (where `state` is null pre-prompt but `agent` is now
    // known). Identity and state are cleared INDEPENDENTLY: `agent: null` is
    // the identity clear (exit-to-`None` reverts the mark to shell), a null
    // state only clears state — it must NOT drop an announced identity, or the
    // Session mark would not render until the first prompt (ADR 0011 amendment).
    const sessionId = (event.session_id as Id | null) ?? null;
    const state = (event.state as AgentState | null) ?? null;
    const agent = (event.agent as KnownAgent | null | undefined) ?? null;
    if (sessionId) {
      applyAgentState(sessionId, state);
      applySessionAgent(sessionId, agent);
    }
  }
  if (event.type === "output-active") {
    const sessionId = event.session_id as Id;
    const active = Boolean(event.active);
    sessionOutputActive.update((current) =>
      current[sessionId] === active ? current : { ...current, [sessionId]: active },
    );
  }
  if (event.type === "session-command") {
    const sessionId = event.session_id as Id;
    const command = (event.command as string | null) ?? null;
    sessionCommands.update((current) => ({ ...current, [sessionId]: command }));
  }
  if (event.type === "job-progress") {
    applyJobProgress(
      event.job_id as Id,
      event.status as JobStatus,
      (event.message as string | null) ?? null,
      (event.kind as string | null) ?? null,
    );
  }
  if (event.type === "job-completed") {
    completeJob(event.job_id as Id, event.response as Response);
  }
  if (event.type === "session-opened") {
    const session = event.session as Session;
    sessions.update((items) => upsert(items, session));
    // Replay the daemon-owned state, announced identity, and output gate on
    // attach so a late-joining window is immediately correct (ADR 0011).
    const state = (event.agent_state as AgentState | null | undefined) ?? null;
    const agent = (event.agent as KnownAgent | null | undefined) ?? null;
    const outputActive = Boolean(event.output_active);
    applyAgentState(session.id, state);
    applySessionAgent(session.id, agent);
    sessionOutputActive.update((current) =>
      current[session.id] === outputActive ? current : { ...current, [session.id]: outputActive },
    );
    activeSessionId.update((current) => current ?? session.id);
    // Reset the ring + (re)register the output channel. On a reconnect the
    // daemon replays the full scrollback right after this event, so the
    // reset keeps that replay from duplicating the prior bytes.
    openSessionOutput(session.id);
  }
  if (event.type === "session-closed") {
    const sessionId = event.session_id as Id;
    // Drop the session locally; per-worktree/project badges derive from the
    // remaining sessions' daemon-owned agent state.
    sessions.update((items) => items.filter((s) => s.id !== sessionId));
    activeSessionId.update((current) => (current === sessionId ? null : current));
    agentStates.update((current) => {
      const next = { ...current };
      delete next[sessionId];
      return next;
    });
    sessionAgents.update((current) => {
      if (!(sessionId in current)) return current;
      const next = { ...current };
      delete next[sessionId];
      return next;
    });
    sessionOutputActive.update((current) => {
      if (!(sessionId in current)) return current;
      const next = { ...current };
      delete next[sessionId];
      return next;
    });
    sessionCommands.update((current) => {
      const next = { ...current };
      delete next[sessionId];
      return next;
    });
    closeSessionOutput(sessionId);

  }
  if (event.type === "project-removed") {
    // A project was forgotten (possibly by another window). Drop it and
    // its worktrees locally; the daemon also broadcasts session-closed for
    // any sessions it killed, so those are pruned above.
    const projectId = event.project_id as Id;
    const removedWorktreeIds = new Set(
      get(worktrees)
        .filter((w) => w.project_id === projectId)
        .map((w) => w.id),
    );
    if (get(selectedProjectId) === projectId) {
      const remaining = get(projects).filter((p) => p.id !== projectId);
      selectedProjectId.set(remaining.length > 0 ? remaining[0].id : null);
      selectedWorktreeId.set(null);
    } else if (removedWorktreeIds.has(get(selectedWorktreeId) as Id)) {
      selectedWorktreeId.set(null);
    }
    projects.update((items) => items.filter((p) => p.id !== projectId));
    worktrees.update((items) => items.filter((w) => w.project_id !== projectId));
  }
}

// ---- connection lifecycle -------------------------------------------------

let unlisteners: UnlistenFn[] = [];
let booted = false;

// Set up the three daemon event subscriptions, then run the connect handshake
// and load the initial snapshot. `connect_daemon` spawns a daemon if none is
// listening, so this doubles as the recovery path after a quit/reboot.
export async function initDaemon(): Promise<void> {
  if (booted) return;
  booted = true;

  connection.set("connecting");
  try {
    unlisteners.push(
      await listen<HitchEvent>("hitch-event", (message) => applyHitchEvent(message.payload)),
    );

    unlisteners.push(
      await listen<{ reason: string }>("hitch-disconnected", (message) => {
        connection.set("offline");
        error.set(message.payload.reason);
        // The daemon link dropped; ephemeral Jobs cannot survive it (ADR 0008).
        failAllJobs(message.payload.reason);
      }),
    );

    // Daemon Status drives the four-state model + the derived connection. The
    // Rust side pushes this on every transition (starting/running/unreachable/
    // failed) with a log-sourced reason on failure (ADR 0009).
    unlisteners.push(
      await listen<{ status: DaemonStatus; reason: string | null; log_path: string }>(
        "hitch-status",
        (message) => {
          applyDaemonStatus(message.payload.status, message.payload.reason);
          daemonLogPath.set(message.payload.log_path);
        },
      ),
    );

    // Auto-recovery re-attached the socket; re-snapshot so projects/worktrees/
    // sessions reflect the live daemon (sessions also replay via events).
    unlisteners.push(
      await listen("hitch-reconnected", () => {
        void refreshAll().catch((err) => error.set(toMessage(err)));
      }),
    );

    // Seed the log path up front so "View log" works even if the first connect
    // fails (the status events also carry it).
    try {
      const snapshot = await invoke<{
        status: DaemonStatus;
        reason: string | null;
        log_path: string;
      }>("get_daemon_status");
      applyDaemonStatus(snapshot.status, snapshot.reason);
      daemonLogPath.set(snapshot.log_path);
    } catch {
      // Non-fatal: the status events populate the path on the next transition.
    }

    await invoke("connect_daemon");
    applyDaemonStatus("running", null);
    await refreshSnapshotAfterConnect();
  } catch (err) {
    applyDaemonStatus("failed", toMessage(err));
  }
}

// Apply a Daemon Status to the stores, deriving the narrower `connection` the
// git-poll guard and the offline banner read. Exported as the seam the
// `hitch-status` listener and the unit tests both drive.
export function applyDaemonStatus(status: DaemonStatus, reason: string | null): void {
  daemonStatus.set(status);
  daemonReason.set(reason);
  if (status === "running") {
    connection.set("ready");
    error.set(null);
    startPrStatusPolling();
  } else if (status === "starting") {
    connection.set("connecting");
  } else {
    // unreachable | failed
    connection.set("offline");
    if (reason) error.set(reason);
    failAllJobs(reason ?? status);
    stopPrStatusPolling();
  }
}

export function disposeDaemon(): void {
  unlisteners.forEach((unlisten) => unlisten());
  unlisteners = [];
  stopGitStatusPolling();
  stopPrStatusPolling();
  for (const sessionId of Array.from(channels.keys())) {
    closeSessionOutput(sessionId);
  }
  rings.clear();
  booted = false;
}

// Re-run the connect handshake on demand — the manual "daemon went away" path.
// Auto-recovery (ADR 0009) usually handles this, but the button stays for a
// daemon the GUI has given up on (crash-loop `failed`).
export async function reconnect(): Promise<void> {
  error.set(null);
  applyDaemonStatus("starting", null);
  try {
    await invoke("connect_daemon");
    applyDaemonStatus("running", null);
    await refreshSnapshotAfterConnect();
  } catch (err) {
    applyDaemonStatus("failed", toMessage(err));
  }
}

// Restart the daemon from the UI (status popover / tray "Restart daemon").
export async function restartDaemon(): Promise<void> {
  error.set(null);
  applyDaemonStatus("starting", null);
  try {
    await invoke("restart_daemon_command");
    applyDaemonStatus("running", null);
    await refreshSnapshotAfterConnect();
  } catch (err) {
    applyDaemonStatus("failed", toMessage(err));
  }
}

// Fetch the daemon log tail for the status popover.
export async function fetchDaemonLogTail(lines = 200): Promise<string | null> {
  try {
    return await invoke<string | null>("get_daemon_log_tail", { lines });
  } catch {
    return null;
  }
}

// Open the daemon log file in the OS default viewer (status popover / tray).
export async function openDaemonLog(): Promise<void> {
  const path = get(daemonLogPath);
  if (!path) return;
  try {
    const { openPath } = await import("@tauri-apps/plugin-opener");
    await openPath(path);
  } catch (err) {
    error.set(toMessage(err));
  }
}

// ---- git status load ------------------------------------------------------

export async function loadGitStatus(worktreeId: Id): Promise<GitStatus> {
  const requestSeq = ++statusRequestSeq;
  const response = await daemonRequest<Response & { status: GitStatus }>({
    type: "git-status",
    worktree_id: worktreeId,
  });
  // A slow status response from a previous worktree/poll must not replace newer
  // UI state. This matters when agents edit files while the user stages changes.
  if (requestSeq === statusRequestSeq && get(gitWorktreeId) === worktreeId) {
    gitStatus.set(response.status);
  }
  dirtyWorktrees.update((current) =>
    current[worktreeId] === response.status.dirty
      ? current
      : { ...current, [worktreeId]: response.status.dirty },
  );
  worktreeLineStats.update((current) => {
    const next = {
      additions: response.status.additions,
      deletions: response.status.deletions,
    };
    const previous = current[worktreeId];
    if (
      previous?.additions === next.additions &&
      previous?.deletions === next.deletions
    ) {
      return current;
    }
    return { ...current, [worktreeId]: next };
  });
  return response.status;
}

// Fetch the PR for a worktree's branch and store it. On-demand only (worktree
// switch + after git ops). A seq guard drops a slow response once the selected
// worktree has moved on, mirroring loadGitStatus. Failures clear to `null` so
// the UI falls back to offering Create-PR rather than getting stuck.
export async function loadPrStatus(worktreeId: Id): Promise<void> {
  const freshnessSeq = ++prByWorktreeSeq;
  // Mark a lookup for this worktree as in flight so a slower, older project-wide
  // response can't clobber it before it resolves (see prByWorktreeStarted).
  prByWorktreeStarted.set(worktreeId, freshnessSeq);
  try {
    const response = await runJob<Response & { pr: PrInfo | null }>(
      { type: "pr-status", worktree_id: worktreeId },
      "pr-status",
    );
    // The result is authoritative for `worktreeId` regardless of which worktree
    // is selected now, so always feed the per-worktree map; the selected
    // worktree's single `prInfo` is gated by the same per-worktree freshness
    // guard so an unrelated project's batched lookup can't drop this result.
    const applied = writePrByWorktree(worktreeId, response.pr ?? null, freshnessSeq);
    if (applied && get(gitWorktreeId) === worktreeId) {
      prInfo.set(response.pr ?? null);
    }
  } catch {
    // A failed lookup is authoritative for this worktree only if no newer lookup
    // has started or landed. Keep the selected action state and sidebar chip in
    // lockstep by clearing both through the same per-worktree freshness guard.
    const applied = writePrByWorktree(worktreeId, null, freshnessSeq);
    if (applied && get(gitWorktreeId) === worktreeId) {
      prInfo.set(null);
    }
  }
}

// PR status is one `gh pr list` per project, so it's network-priced; refreshAll
// can fire often (after every git op), so throttle per project. `gh pr list`
// rarely changes faster than this between refreshes, and a worktree switch still
// fetches its own fresh status via loadPrStatus.
const PR_STATUS_MIN_INTERVAL_MS = 20_000;
const lastProjectPrFetch = new Map<Id, number>();
// Projects with a `project-pr-statuses` job still pending. `runJob` has no
// timeout, so a hung/slow `gh pr list` keeps its promise open indefinitely; the
// forced periodic poll bypasses the throttle, so without this guard every tick
// would start another daemon job and another `gh` process for the same repo.
const projectPrInFlight = new Set<Id>();

// Populate the PR chip for EVERY worktree of a project from a single batched
// lookup, so chips appear without visiting each worktree. Best-effort and
// fire-and-forget from refreshAll; a failure just clears the throttle so the
// next refresh can retry sooner.
export async function loadProjectPrStatuses(
  projectId: Id,
  options: { force?: boolean } = {},
): Promise<void> {
  const now = Date.now();
  if (!options.force && now - (lastProjectPrFetch.get(projectId) ?? 0) < PR_STATUS_MIN_INTERVAL_MS) {
    return;
  }
  // A previous lookup for this project is still pending (slow/hung `gh`). Forced
  // polls bypass the throttle, so without this we'd keep stacking jobs.
  if (projectPrInFlight.has(projectId)) {
    return;
  }
  projectPrInFlight.add(projectId);
  lastProjectPrFetch.set(projectId, now);
  // Stamp before awaiting: a project-wide response that started before a newer
  // per-worktree `loadPrStatus` must not clobber that fresher status. The same
  // stamp gates the selected worktree's single `prInfo`, mirroring loadPrStatus.
  const freshnessSeq = ++prByWorktreeSeq;
  try {
    const response = await runJob<
      Response & { statuses: { worktree_id: Id; pr: PrInfo | null }[] }
    >({ type: "project-pr-statuses", project_id: projectId }, "pr-status");
    const selectedId = get(gitWorktreeId);
    for (const status of response.statuses) {
      const applied = writePrByWorktree(status.worktree_id, status.pr ?? null, freshnessSeq);
      // Keep the selected worktree's action state (Create vs Open PR) in step with
      // its sidebar chip; both now flow from the same batched lookup. Gated by the
      // per-worktree freshness guard so a slower batched response can't regress a
      // newer per-worktree result.
      if (applied && selectedId === status.worktree_id) {
        prInfo.set(status.pr ?? null);
      }
    }
  } catch {
    lastProjectPrFetch.delete(projectId);
  } finally {
    projectPrInFlight.delete(projectId);
  }
}

function stopGitStatusPolling(): void {
  if (statusPollTimer) clearInterval(statusPollTimer);
  statusPollTimer = null;
  statusPollInFlight = false;
}

function pollSelectedGitStatus(worktreeId: Id): void {
  if (statusPollInFlight || get(connection) !== "ready" || get(gitBusy)) return;
  if (get(gitWorktreeId) !== worktreeId) return;
  statusPollInFlight = true;
  void loadGitStatus(worktreeId)
    .catch(() => {})
    .finally(() => {
      statusPollInFlight = false;
    });
}

function startGitStatusPolling(worktreeId: Id): void {
  stopGitStatusPolling();
  statusPollTimer = setInterval(() => pollSelectedGitStatus(worktreeId), STATUS_POLL_MS);
}

// Refresh PR chips for every git-backed project. The periodic poll forces past
// the refreshAll throttle (it is itself paced by PR_POLL_MS); the focus refresh
// does not, so rapid window switching can't hammer `gh`.
function pollAllProjectPrStatuses(options: { force?: boolean } = {}): void {
  if (get(connection) !== "ready") return;
  for (const project of get(projects)) {
    if (project.kind === "git-backed") {
      void loadProjectPrStatuses(project.id, options);
    }
  }
}

function startPrStatusPolling(): void {
  stopPrStatusPolling();
  prPollTimer = setInterval(() => {
    // Don't poll a backgrounded app: the focus refresh below catches us up the
    // moment the user returns, so a hidden window spawns no `gh` processes.
    if (typeof document !== "undefined" && document.hidden) return;
    pollAllProjectPrStatuses({ force: true });
  }, PR_POLL_MS);
  if (typeof window !== "undefined" && !prFocusHandler) {
    prFocusHandler = () => pollAllProjectPrStatuses();
    window.addEventListener("focus", prFocusHandler);
  }
}

function stopPrStatusPolling(): void {
  if (prPollTimer) clearInterval(prPollTimer);
  prPollTimer = null;
  if (typeof window !== "undefined" && prFocusHandler) {
    window.removeEventListener("focus", prFocusHandler);
    prFocusHandler = null;
  }
}

function statusAfterStage(status: FileStatus): FileStatus {
  return status === "untracked" ? "added" : status;
}

function statusAfterUnstage(status: FileStatus): FileStatus {
  return status === "added" ? "untracked" : status;
}

function optimisticallySetFilesStaged(
  worktreeId: Id,
  paths: string[],
  staged: boolean,
): void {
  const selected = new Set(paths);
  gitStatus.update((current) => {
    if (!current || current.worktree_id !== worktreeId) return current;
    let changed = false;
    const files: ChangedFile[] = current.files.map((file) => {
      if (!selected.has(file.path)) return file;
      const next = {
        ...file,
        staged,
        status: staged ? statusAfterStage(file.status) : statusAfterUnstage(file.status),
      };
      changed ||= next.staged !== file.staged || next.status !== file.status;
      return next;
    });
    return changed ? { ...current, dirty: files.length > 0, files } : current;
  });
  dirtyWorktrees.update((current) =>
    current[worktreeId] === true ? current : { ...current, [worktreeId]: true },
  );
}

// ---- request actions ------------------------------------------------------

// The form/confirmation dialogs (add-project, clone, create/remove worktree,
// create-PR) throw on failure so they can surface the error inline and only
// dismiss on success. The fire-and-forget actions below (open/rename/close
// session, stage, commit, push) instead swallow into the `error` store.
export async function addProject(root: string): Promise<void> {
  const trimmed = root.trim();
  if (!trimmed) return;
  await daemonRequest({ type: "add-project", root: trimmed });
  await refreshAll();
}

// Open the native folder picker and add the chosen directory as a project.
// This stays the primary local add-project flow; the separate dialog fallback
// handles manual path entry when the picker is unavailable or unsuitable.
// Cancelling is a silent no-op; failures surface in the `error` store.
export async function pickAndAddProject(): Promise<void> {
  try {
    const picked = await open({
      directory: true,
      multiple: false,
      title: "Add a project folder",
    });
    if (typeof picked !== "string") return;
    await addProject(picked);
  } catch (err) {
    error.set(toMessage(err));
  }
}

export async function cloneProject(
  remoteUrl: string,
  destination: string,
  name: string | null = null,
): Promise<void> {
  await runJob(
    {
      type: "clone-project",
      remote_url: remoteUrl.trim(),
      destination: destination.trim(),
      name: name?.trim() || null,
    },
    "clone",
  );
  await refreshAll();
}

export async function removeProject(projectId: Id, force: boolean): Promise<void> {
  await daemonRequest({
    type: "remove-project",
    project_id: projectId,
    force,
  });
  if (get(selectedProjectId) === projectId) {
    selectedProjectId.set(null);
    selectedWorktreeId.set(null);
  }
  await refreshAll();
}

export async function listBranches(projectId: Id): Promise<BranchSummary[]> {
  const response = await daemonRequest<Response & { branches: BranchSummary[] }>({
    type: "list-branches",
    project_id: projectId,
  });
  return response.branches;
}

export async function createWorktree(
  projectId: Id,
  branch: string,
  base: string | null = null,
  mode: "new-branch" | "existing-branch" = "new-branch",
): Promise<Worktree | null> {
  const trimmed = branch.trim();
  if (!trimmed) return null;
  const response = await runJob<Response & { worktrees: Worktree[] }>(
    {
      type: "create-worktree",
      project_id: projectId,
      branch: trimmed,
      base,
      mode,
    },
    "create-worktree",
  );
  const created = response.worktrees[0] ?? null;
  if (created) {
    worktrees.update((items) => upsert(items, created));
    selectedWorktreeId.set(created.id);
  }
  return created;
}

// `force` overrides the daemon's dirty-worktree / live-session guards — the
// remove dialog confirms exactly those cases before calling. `delete_branch`
// is gated by ADR 0001 (only when merged); the frozen contract carries no
// merge status, so the dialog keeps that option disabled and passes `false`.
export async function removeWorktree(
  worktreeId: Id,
  deleteBranch: boolean,
  force: boolean,
): Promise<void> {
  await daemonRequest({
    type: "remove-worktree",
    worktree_id: worktreeId,
    delete_branch: deleteBranch,
    force,
  });
  removeWorktreeLocal(worktreeId);
}

// Last grid an active Terminal successfully fitted to, in cols/rows. Used to
// open the NEXT session's PTY at the right size so it doesn't reflow on first
// fit. Updated by Terminal.svelte after every successful fit; null until the
// first terminal has rendered (the very first open then estimates from the DOM).
let lastTerminalSize: { cols: number; rows: number } | null = null;

// Terminal.svelte calls this after a successful fit with its live term.cols/rows.
export function recordTerminalSize(cols: number, rows: number): void {
  if (cols > 0 && rows > 0) lastTerminalSize = { cols, rows };
}

// The xterm config in Terminal.svelte. Kept here so the offscreen measuring
// span below uses the exact same font as the real terminal.
const TERM_FONT_FAMILY =
  '"Berkeley Mono", ui-monospace, "SF Mono", "JetBrains Mono", Menlo, monospace';
const TERM_FONT_SIZE_PX = 12.5;
// `.term` padding from Terminal.svelte's stylesheet (`padding: 12px 14px`).
const TERM_PADDING_X = 14;
const TERM_PADDING_Y = 12;
// Last-resort grid when nothing on the page is measurable (e.g. opening a
// session before any view has laid out). A sane terminal-ish default.
const FALLBACK_COLS = 120;
const FALLBACK_ROWS = 32;

// Estimate the grid a freshly-opened terminal will fit to, so its PTY spawns at
// (close to) that size. We measure the visible center view area and divide by
// the cell size derived from the xterm font via an offscreen probe `<span>`.
// This is only the FIRST-open path; every subsequent open reuses the exact
// `lastTerminalSize` recorded by a live terminal, so the estimate just needs to
// be in the right ballpark to avoid a jarring reflow.
function estimateInitialSize(): { cols: number; rows: number } {
  if (typeof document === "undefined") {
    return { cols: FALLBACK_COLS, rows: FALLBACK_ROWS };
  }
  // The center pane is where terminals render; fall back to body if absent.
  const view =
    document.querySelector(".view") ??
    document.querySelector(".center") ??
    document.body;
  const rect = view?.getBoundingClientRect();
  if (!rect || rect.width === 0 || rect.height === 0) {
    return { cols: FALLBACK_COLS, rows: FALLBACK_ROWS };
  }

  // Offscreen probe with the terminal's exact font. `ch`-style width: measure a
  // wide run of a monospace glyph and divide, so sub-pixel advance is averaged.
  const probe = document.createElement("span");
  probe.style.position = "absolute";
  probe.style.visibility = "hidden";
  probe.style.whiteSpace = "pre";
  probe.style.fontFamily = TERM_FONT_FAMILY;
  probe.style.fontSize = `${TERM_FONT_SIZE_PX}px`;
  probe.style.lineHeight = "normal";
  probe.textContent = "0".repeat(100);
  document.body.appendChild(probe);
  const cellWidth = probe.getBoundingClientRect().width / 100;
  const cellHeight = probe.getBoundingClientRect().height;
  probe.remove();

  if (cellWidth <= 0 || cellHeight <= 0) {
    return { cols: FALLBACK_COLS, rows: FALLBACK_ROWS };
  }
  // Subtract the `.term` padding the content area loses on both sides.
  const usableWidth = rect.width - TERM_PADDING_X * 2;
  const usableHeight = rect.height - TERM_PADDING_Y * 2;
  const cols = Math.max(1, Math.floor(usableWidth / cellWidth));
  const rows = Math.max(1, Math.floor(usableHeight / cellHeight));
  return { cols, rows };
}

// `command` is an argv (e.g. ["claude"]); null spawns the default shell. This
// mirrors the daemon's OpenSession.command (Option<Vec<String>>) contract.
// `cols`/`rows` carry the initial grid so the PTY spawns at the right size and
// the terminal doesn't visibly reflow on its first fit.
export async function openSession(
  parent: SessionParent,
  name: string,
  command: string[] | null = null,
): Promise<Session | null> {
  try {
    error.set(null);
    const { cols, rows } = lastTerminalSize ?? estimateInitialSize();
    const response = await daemonRequest<Response & { session: Session }>({
      type: "open-session",
      parent,
      name: name.trim() || "shell",
      command,
      cols,
      rows,
    });
    activeSessionId.set(response.session.id);
    if (command?.[0]) {
      sessionCommands.update((current) => ({ ...current, [response.session.id]: command[0] }));
    }
    return response.session;
  } catch (err) {
    error.set(toMessage(err));
    return null;
  }
}

export async function renameSession(session: Session, name: string): Promise<void> {
  const next = name.trim();
  if (!next || next === session.name) return;
  try {
    error.set(null);
    await daemonRequest({
      type: "rename-session",
      session_id: session.id,
      name: next,
    });
    sessions.update((items) =>
      items.map((item) => (item.id === session.id ? { ...item, name: next } : item)),
    );
  } catch (err) {
    error.set(toMessage(err));
  }
}

export async function closeSession(session: Session): Promise<void> {
  try {
    error.set(null);
    await daemonRequest({
      type: "close-session",
      session_id: session.id,
      kill_process: true,
    });
    sessions.update((items) => items.filter((item) => item.id !== session.id));
    closeSessionOutput(session.id);
  } catch (err) {
    error.set(toMessage(err));
  }
}

export async function resizeSession(
  sessionId: Id,
  cols: number,
  rows: number,
): Promise<void> {
  try {
    await daemonRequest({ type: "resize-session", session_id: sessionId, cols, rows });
  } catch (err) {
    // Resize is best-effort: NEVER throw, so keystrokes keep flowing even if
    // the PTY exited mid-resize. But a dropped final size is a real bug (the
    // child renders at the wrong grid), so log it — a silent swallow hid lost
    // resizes. The debounce trailing edge still chains a repaint afterward.
    console.warn(`resize-session failed for ${sessionId} (${cols}x${rows})`, err);
  }
}

// Ask the daemon to force the session's PTY child to redraw a clean full frame
// (it replies with an Ack). Used right after a settled resize and on tab
// re-activation so a TUI like Claude Code repaints crisply at the new/current
// grid. Best-effort: a missing or dead PTY must never break the UI, so all
// errors are swallowed. Emits the agreed `{ type, session_id }` contract.
export async function repaintSession(sessionId: Id): Promise<void> {
  try {
    const request: RepaintSessionRequest = {
      type: "repaint-session",
      session_id: sessionId,
    };
    await daemonRequest(request);
  } catch {
    // Repaint is best-effort; a missing/dead PTY must not break the UI.
  }
}

// Trailing debounce for the daemon resize notification, keyed per session.
// xterm's local fit() reflows smoothly on every ResizeObserver tick, but the
// PTY child only needs the FINAL size: dragging the window or toggling a panel
// fires a storm of ticks, and telling the child about each intermediate size
// floods it with SIGWINCH and garbles a running TUI (vim/lazygit). We coalesce
// those into ONE `resize-session` request that fires ~70 ms after the size
// settles. Per session so two visible terminals don't share a timer.
const RESIZE_DEBOUNCE_MS = 70;
const resizeTimers = new Map<Id, ReturnType<typeof setTimeout>>();

export function resizeSessionDebounced(
  sessionId: Id,
  cols: number,
  rows: number,
): void {
  const existing = resizeTimers.get(sessionId);
  if (existing) clearTimeout(existing);
  resizeTimers.set(
    sessionId,
    setTimeout(() => {
      resizeTimers.delete(sessionId);
      // Lossless settle: the final size always reaches the daemon, and once it
      // has LANDED we force a repaint so the child redraws clean at the new
      // grid. `.finally` runs the repaint even if the resize errored — the
      // settled-frame repaint is the whole point and must not be skipped.
      void resizeSession(sessionId, cols, rows).finally(() => {
        void repaintSession(sessionId);
      });
    }, RESIZE_DEBOUNCE_MS),
  );
}

// Drop any pending debounced resize for a session that is going away, so its
// trailing timer can't fire a request against a dead PTY.
function clearResizeDebounce(sessionId: Id): void {
  const existing = resizeTimers.get(sessionId);
  if (existing) {
    clearTimeout(existing);
    resizeTimers.delete(sessionId);
  }
}

export function sendInput(sessionId: Id, data: string): void {
  void invoke("send_session_input", { sessionId, data });
}

// `activate` opens the diff as the center view (a user clicking a changed
// file). The keep-in-sync refresh from staging passes `false` so re-fetching a
// diff never yanks the view away from a terminal the user is looking at.
export async function viewDiff(path: string, activate = true): Promise<void> {
  const worktreeId = get(gitWorktreeId);
  if (!worktreeId) return;
  const requestSeq = ++diffRequestSeq;
  const cacheKey = `${worktreeId}\0${path}`;
  diffPath.set(path);
  diffText.set(diffCache.get(cacheKey) ?? null);
  if (activate) diffActive.set(true);
  try {
    const response = await daemonRequest<Response & { diff: { diff: string } }>({
      type: "git-diff",
      worktree_id: worktreeId,
      path,
    });
    diffCache.set(cacheKey, response.diff.diff);
    if (
      requestSeq === diffRequestSeq &&
      get(gitWorktreeId) === worktreeId &&
      get(diffPath) === path
    ) {
      diffText.set(response.diff.diff);
    }
  } catch (err) {
    error.set(toMessage(err));
    if (
      requestSeq === diffRequestSeq &&
      get(gitWorktreeId) === worktreeId &&
      get(diffPath) === path
    ) {
      diffText.set(null);
    }
  }
}

// Close the diff tab and fall back to the active session's terminal.
export function closeDiff(): void {
  diffActive.set(false);
  diffPath.set(null);
  diffText.set(null);
}

export async function setFilesStaged(
  paths: string[],
  staged: boolean,
  worktreeId: Id | null = get(gitWorktreeId),
): Promise<void> {
  if (!worktreeId || paths.length === 0) return;
  const before = get(gitStatus);
  // Invalidate any in-flight status poll so it cannot briefly undo the
  // immediate stage/unstage feedback.
  statusRequestSeq += 1;
  optimisticallySetFilesStaged(worktreeId, paths, staged);
  gitBusy.set(true);
  try {
    error.set(null);
    await daemonRequest({
      type: staged ? "stage-files" : "unstage-files",
      worktree_id: worktreeId,
      paths,
    });
    gitBusy.set(false);
    void loadGitStatus(worktreeId).catch((err) => error.set(toMessage(err)));
    // Keep an open diff in sync if its file was just (un)staged, without
    // stealing focus from a terminal the user may be looking at.
    const open = get(diffPath);
    if (open && paths.includes(open)) {
      diffCache.delete(`${worktreeId}\0${open}`);
      void viewDiff(open, false);
    }
  } catch (err) {
    error.set(toMessage(err));
    if (before?.worktree_id === worktreeId && get(gitWorktreeId) === worktreeId) {
      gitStatus.set(before);
    }
    void loadGitStatus(worktreeId).catch((refreshErr) => error.set(toMessage(refreshErr)));
    // Rethrow so awaiting callers (e.g. CommitDialog's stage-all-and-generate)
    // can stop their flow instead of proceeding on a failed stage. The error
    // store + optimistic rollback above still surface the failure on their own,
    // so fire-and-forget callers must `.catch()` (see RightRail).
    throw err;
  } finally {
    gitBusy.set(false);
  }
}

export function setFileStaged(path: string, staged: boolean): Promise<void> {
  return setFilesStaged([path], staged);
}

export async function discardFiles(paths: string[]): Promise<void> {
  const worktreeId = get(gitWorktreeId);
  if (!worktreeId || paths.length === 0) return;
  statusRequestSeq += 1;
  gitBusy.set(true);
  try {
    error.set(null);
    await daemonRequest({
      type: "discard-files",
      worktree_id: worktreeId,
      paths,
    });
    const open = get(diffPath);
    for (const path of paths) diffCache.delete(`${worktreeId}\0${path}`);
    if (open && paths.includes(open)) closeDiff();
    await loadGitStatus(worktreeId);
  } catch (err) {
    error.set(toMessage(err));
    void loadGitStatus(worktreeId).catch((refreshErr) => error.set(toMessage(refreshErr)));
  } finally {
    gitBusy.set(false);
  }
}

export function discardFile(path: string): Promise<void> {
  return discardFiles([path]);
}

export function discardAllFiles(): Promise<void> {
  const paths = get(gitStatus)?.files.map((file) => file.path) ?? [];
  return discardFiles(paths);
}

export async function commit(
  subject: string,
  body: string | null = null,
  worktreeId: Id | null = get(gitWorktreeId),
): Promise<void> {
  const trimmedSubject = subject.trim();
  const trimmedBody = body?.trim() || null;
  if (!worktreeId || !trimmedSubject) return;
  gitBusy.set(true);
  try {
    error.set(null);
    await daemonRequest({
      type: "commit",
      worktree_id: worktreeId,
      subject: trimmedSubject,
      body: trimmedBody,
    });
    closeDiff();
    await loadGitStatus(worktreeId);
  } catch (err) {
    error.set(toMessage(err));
    throw err;
  } finally {
    gitBusy.set(false);
  }
}

export async function listDraftModels(
  provider: DraftProvider,
  paths: { claudePath?: string; codexPath?: string } = {},
): Promise<string[]> {
  const response = await runJob<Response & { models: string[] }>(
    { type: "list-draft-models", provider, settings: draftDiscoverySettings(provider, paths) },
    "draft-models",
  );
  return response.models;
}

export async function generateCommitDraft(
  worktreeId: Id | null = get(gitWorktreeId),
): Promise<CommitDraft> {
  if (!worktreeId) throw new Error("Select a git worktree first.");
  const response = await runJob<Response & { draft: CommitDraft }>(
    {
      type: "generate-commit-draft",
      worktree_id: worktreeId,
      settings: draftGenerationSettings(),
    },
    "commit-draft",
  );
  return response.draft;
}

export async function generatePullRequestDraft(base: string | null): Promise<PullRequestDraft> {
  const worktreeId = get(gitWorktreeId);
  if (!worktreeId) throw new Error("Select a git worktree first.");
  const response = await runJob<Response & { draft: PullRequestDraft }>(
    {
      type: "generate-pull-request-draft",
      worktree_id: worktreeId,
      base: base?.trim() || null,
      settings: draftGenerationSettings(),
    },
    "pr-draft",
  );
  return response.draft;
}

function draftDiscoverySettings(
  provider: DraftProvider,
  paths: { claudePath?: string; codexPath?: string } = {},
): DraftGenerationSettings | null {
  if (provider === "stub") return null;
  return draftSettingsForProvider(provider, null, paths);
}

function draftGenerationSettings(): DraftGenerationSettings | null {
  const provider = get(draftProvider);
  // No explicit desktop choice → omit settings so the daemon keeps its own
  // configured provider/model/path defaults instead of being forced to "stub".
  if (!provider) return null;
  return draftSettingsForProvider(provider, get(draftModel).trim() || null);
}

function draftSettingsForProvider(
  provider: DraftProvider,
  model: string | null,
  paths: { claudePath?: string; codexPath?: string } = {},
): DraftGenerationSettings {
  const claudePath = (paths.claudePath ?? get(draftClaudePath)).trim() || null;
  const codexPath = (paths.codexPath ?? get(draftCodexPath)).trim() || null;
  return {
    provider,
    model,
    claude_path: claudePath,
    codex_path: codexPath,
  };
}

export async function push(worktreeId: Id | null = get(gitWorktreeId)): Promise<void> {
  if (!worktreeId) return;
  gitBusy.set(true);
  try {
    error.set(null);
    await runJob({ type: "push", worktree_id: worktreeId }, "push");
  } catch (err) {
    error.set(toMessage(err));
    throw err;
  } finally {
    gitBusy.set(false);
  }
}

export async function fetchRemote(worktreeId: Id | null = get(gitWorktreeId)): Promise<void> {
  if (!worktreeId) return;
  gitBusy.set(true);
  try {
    error.set(null);
    await runJob({ type: "fetch", worktree_id: worktreeId }, "fetch");
  } catch (err) {
    error.set(toMessage(err));
    throw err;
  } finally {
    gitBusy.set(false);
  }
}

export async function pull(worktreeId: Id | null = get(gitWorktreeId)): Promise<void> {
  if (!worktreeId) return;
  gitBusy.set(true);
  try {
    error.set(null);
    await runJob({ type: "pull", worktree_id: worktreeId }, "pull");
  } catch (err) {
    error.set(toMessage(err));
    throw err;
  } finally {
    gitBusy.set(false);
  }
}

// Throws on failure so the dialog can surface the error inline (mirrors App.tsx).
export async function createPr(fields: PrFields): Promise<void> {
  const worktreeId = get(gitWorktreeId);
  if (!worktreeId) throw new Error("Select a git worktree first.");
  if (get(gitBusy)) throw new Error("Wait for the current git operation to finish.");
  gitBusy.set(true);
  try {
    const response = await runJob<Response & { url: string }>(
      {
        type: "create-pull-request",
        worktree_id: worktreeId,
        title: fields.title,
        body: fields.body,
        base: fields.base,
        draft: fields.draft,
      },
      "create-pr",
    );
    prUrl.set(response.url);
    // Refresh so the action menu flips from "Create PR" to "Open PR".
    await loadPrStatus(worktreeId);
  } finally {
    gitBusy.set(false);
  }
}

// ---- selection fix-up + cleanup (run once, here, as subscriptions) --------

// Fall back to the first project when nothing is selected yet.
projects.subscribe(($projects) => {
  if (!get(selectedProjectId) && $projects.length > 0) {
    selectedProjectId.set($projects[0].id);
  }
});

// Keep the selected worktree valid for the selected project, but never auto-
// pick one. Plain projects have no worktrees; switching git projects (or
// removing the selected worktree) invalidates the current selection — in both
// cases we clear it to null rather than jumping into `main`. Clicking a project
// then expands it and shows the "choose a worktree" state; the user picks the
// worktree explicitly, so `main` is no longer special on selection.
derived([selectedProject, projectWorktrees], (v) => v).subscribe(
  ([$project, $worktrees]) => {
    const selected = get(selectedWorktreeId);
    if (selected === null) return;
    if ($project?.kind === "plain") {
      selectedWorktreeId.set(null);
      return;
    }
    if ($project && !$worktrees.some((w) => w.id === selected)) {
      selectedWorktreeId.set(null);
    }
  },
);

// Per-parent memory of the last active session id, so switching worktrees
// (or projects) and coming back restores the tab the user had selected
// instead of always snapping to the first session in the new parent.
const lastActiveByParent = new Map<string, Id>();

// Track the current parent so we can detect parent changes vs same-parent
// session list updates (open/close/rename) and only consult the remembered
// id on a real switch.
let lastParentKey: string | null = null;

// On every parent switch, restore the remembered session for that parent
// (if it's still live), else fall back to the first visible one.
selectedParent.subscribe(($parent) => {
  const key = $parent ? parentKey($parent) : null;
  if (key === lastParentKey) return;
  lastParentKey = key;
  if (!$parent) {
    activeSessionId.set(null);
    return;
  }
  const visible = get(sessions).filter((s) => sessionBelongsTo(s, $parent));
  const remembered = lastActiveByParent.get(key!);
  const restore =
    (remembered && visible.some((s) => s.id === remembered) ? remembered : null) ??
    visible[0]?.id ??
    null;
  activeSessionId.set(restore);
});

// Within the current parent, keep the active session valid if a session
// closes or the list changes; don't fight the parent-switch logic above.
visibleSessions.subscribe(($visible) => {
  const active = get(activeSessionId);
  if (active && $visible.some((s) => s.id === active)) return;
  activeSessionId.set($visible[0]?.id ?? null);
});

// Remember the user's active choice per parent so we can restore it later.
activeSessionId.subscribe(($id) => {
  const parent = get(selectedParent);
  if (!parent || !$id) return;
  lastActiveByParent.set(parentKey(parent), $id);
});

// Drop remembered ids for sessions that have closed.
sessions.subscribe(($sessions) => {
  const liveIds = new Set($sessions.map((s) => s.id));
  for (const [key, id] of lastActiveByParent) {
    if (!liveIds.has(id)) lastActiveByParent.delete(key);
  }
});

// Forget agent state, announced identity, output gate, and running command for
// sessions that have closed (or were dropped on a reconnect), so stale labels
// never linger on the tree or tabs.
sessions.subscribe(($sessions) => {
  const liveIds = new Set($sessions.map((s) => s.id));
  function pruneToLive<T>(current: Record<Id, T>): Record<Id, T> {
    const next = Object.fromEntries(
      Object.entries(current).filter(([id]) => liveIds.has(id)),
    ) as Record<Id, T>;
    return Object.keys(next).length === Object.keys(current).length ? current : next;
  }
  agentStates.update(pruneToLive);
  sessionAgents.update(pruneToLive);
  sessionOutputActive.update(pruneToLive);
  sessionCommands.update(pruneToLive);
});

// Reset the per-worktree Git view state and (re)load status whenever the target
// git worktree changes. Selecting a different worktree clears the open diff and
// commit draft; live dirty refreshes are driven by the worktree-dirty event.
let lastGitWorktreeId: Id | null = null;
gitWorktreeId.subscribe(($id) => {
  if ($id === lastGitWorktreeId) return;
  lastGitWorktreeId = $id;
  gitStatus.set(null);
  closeDiff();
  prUrl.set(null);
  prInfo.set(null);
  stopGitStatusPolling();
  if ($id) {
    void loadGitStatus($id).catch((err) => error.set(toMessage(err)));
    void loadPrStatus($id);
    startGitStatusPolling($id);
  }
});
