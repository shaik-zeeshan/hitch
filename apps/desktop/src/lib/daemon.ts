// Data layer — the single owner of the daemon connection and the reactive app
// state behind the shell. Ported from the provisional React client (App.tsx);
// the request `type` strings, Tauri command names, and derived rollups are the
// contract — copied here, not redesigned. See docs/adr/0006-frontend-stack.md.
//
// State lives in Svelte stores so any component can read it with `$store` and
// the cross-cutting fix-up logic (selection fallbacks, stale-state cleanup)
// runs once here as store subscriptions rather than per-component effects.

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { derived, get, writable } from "svelte/store";
import {
  aggregateAgentState,
  sessionBelongsTo,
  type AgentState,
  type ChangedFile,
  type CommitDraft,
  type FileStatus,
  type GitStatus,
  type HitchEvent,
  type Id,
  type PrFields,
  type PullRequestDraft,
  type Project,
  type Request,
  type Response,
  type Session,
  type SessionOutputPayload,
  type SessionParent,
  type Worktree,
} from "./types";
import { draftModel, draftProvider, type DraftProvider } from "./settings";

export type Connection = "connecting" | "ready" | "offline";

// ---- base stores ----------------------------------------------------------

export const connection = writable<Connection>("connecting");
export const error = writable<string | null>(null);

export const projects = writable<Project[]>([]);
export const worktrees = writable<Worktree[]>([]);
export const sessions = writable<Session[]>([]);

export const buffers = writable<Record<Id, string>>({});
export const agentStates = writable<Record<Id, AgentState>>({});
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

const diffCache = new Map<string, string>();
let diffRequestSeq = 0;
let statusRequestSeq = 0;
let statusPollTimer: ReturnType<typeof setInterval> | null = null;
let statusPollInFlight = false;

const STATUS_POLL_MS = 1_000;

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

// Roll up per-session agent state to the worktree and project rows so a
// collapsed tree still shows which branch needs attention.
export const agentStateByWorktree = derived(
  [worktrees, sessions, agentStates],
  ([$worktrees, $sessions, $agentStates]) => {
    const map: Record<Id, AgentState> = {};
    for (const worktree of $worktrees) {
      const agg = aggregateAgentState(
        $sessions
          .filter((s) => s.parent.kind === "worktree" && s.parent.id === worktree.id)
          .map((s) => $agentStates[s.id]),
      );
      if (agg) map[worktree.id] = agg;
    }
    return map;
  },
);

export const agentStateByProject = derived(
  [projects, worktrees, sessions, agentStates],
  ([$projects, $worktrees, $sessions, $agentStates]) => {
    const map: Record<Id, AgentState> = {};
    for (const project of $projects) {
      const projectWorktreeIds = new Set(
        $worktrees.filter((w) => w.project_id === project.id).map((w) => w.id),
      );
      const agg = aggregateAgentState(
        $sessions
          .filter(
            (s) =>
              (s.parent.kind === "project" && s.parent.id === project.id) ||
              (s.parent.kind === "worktree" && projectWorktreeIds.has(s.parent.id)),
          )
          .map((s) => $agentStates[s.id]),
      );
      if (agg) map[project.id] = agg;
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

function upsert<T extends { id: Id }>(items: T[], item: T): T[] {
  return items.some((existing) => existing.id === item.id)
    ? items.map((existing) => (existing.id === item.id ? item : existing))
    : [...items, item];
}

// ---- snapshot / refresh ---------------------------------------------------

export async function refreshAll(): Promise<void> {
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

  const sessionResponse = await daemonRequest<Response & { sessions: Session[] }>({
    type: "list-sessions",
    parent: null,
  });
  sessions.set(sessionResponse.sessions);

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
      await listen<HitchEvent>("hitch-event", (message) => {
        const event = message.payload;
        if (event.type === "project-updated") {
          projects.update((items) => upsert(items, event.project as Project));
        }
        if (event.type === "worktree-updated") {
          worktrees.update((items) => upsert(items, event.worktree as Worktree));
        }
        if (event.type === "worktree-dirty") {
          const worktreeId = event.worktree_id as Id;
          const dirty = event.dirty as boolean;
          dirtyWorktrees.update((current) =>
            current[worktreeId] === dirty
              ? current
              : { ...current, [worktreeId]: dirty },
          );
          // Refresh the full status so the tree's line stats and, when selected,
          // the Changes panel track filesystem changes live.
          void loadGitStatus(worktreeId).catch(() => {});
        }
        if (event.type === "agent-state") {
          const sessionId = event.session_id as Id | null;
          const state = event.state as AgentState;
          if (sessionId) {
            agentStates.update((current) => ({ ...current, [sessionId]: state }));
          }
        }
        if (event.type === "session-command") {
          const sessionId = event.session_id as Id;
          const command = (event.command as string | null) ?? null;
          sessionCommands.update((current) => ({ ...current, [sessionId]: command }));
        }
        if (event.type === "session-opened") {
          const session = event.session as Session;
          sessions.update((items) => upsert(items, session));
          activeSessionId.update((current) => current ?? session.id);
        }
        if (event.type === "session-closed") {
          const sessionId = event.session_id as Id;
          sessions.update((items) => items.filter((s) => s.id !== sessionId));
          activeSessionId.update((current) =>
            current === sessionId ? null : current,
          );
          sessionCommands.update((current) => {
            const next = { ...current };
            delete next[sessionId];
            return next;
          });
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
          } else if (
            removedWorktreeIds.has(get(selectedWorktreeId) as Id)
          ) {
            selectedWorktreeId.set(null);
          }
          projects.update((items) => items.filter((p) => p.id !== projectId));
          worktrees.update((items) =>
            items.filter((w) => w.project_id !== projectId),
          );
        }
      }),
    );

    unlisteners.push(
      await listen<SessionOutputPayload>("hitch-session-output", (message) => {
        const { session_id, data } = message.payload;
        buffers.update((current) => ({
          ...current,
          [session_id]: `${current[session_id] ?? ""}${data}`,
        }));
      }),
    );

    unlisteners.push(
      await listen<{ reason: string }>("hitch-disconnected", (message) => {
        connection.set("offline");
        error.set(message.payload.reason);
      }),
    );

    await invoke("connect_daemon");
    connection.set("ready");
    await refreshAll();
  } catch (err) {
    connection.set("offline");
    error.set(toMessage(err));
  }
}

export function disposeDaemon(): void {
  unlisteners.forEach((unlisten) => unlisten());
  unlisteners = [];
  stopGitStatusPolling();
  booted = false;
}

// Re-run the connect handshake on demand — the "daemon went away" recovery path.
export async function reconnect(): Promise<void> {
  error.set(null);
  connection.set("connecting");
  try {
    await invoke("connect_daemon");
    connection.set("ready");
    await refreshAll();
  } catch (err) {
    connection.set("offline");
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

export async function cloneProject(
  remoteUrl: string,
  destination: string,
  name: string | null = null,
): Promise<void> {
  await daemonRequest({
    type: "clone-project",
    remote_url: remoteUrl.trim(),
    destination: destination.trim(),
    name: name?.trim() || null,
  });
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

export async function createWorktree(
  projectId: Id,
  branch: string,
  base: string | null = null,
  mode: "new-branch" | "existing-branch" = "new-branch",
): Promise<Worktree | null> {
  const trimmed = branch.trim();
  if (!trimmed) return null;
  const response = await daemonRequest<Response & { worktrees: Worktree[] }>({
    type: "create-worktree",
    project_id: projectId,
    branch: trimmed,
    base,
    mode,
  });
  const created = response.worktrees[0] ?? null;
  if (created) selectedWorktreeId.set(created.id);
  await refreshAll();
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
  if (get(selectedWorktreeId) === worktreeId) selectedWorktreeId.set(null);
  await refreshAll();
}

// `command` is an argv (e.g. ["claude"]); null spawns the default shell. This
// mirrors the daemon's OpenSession.command (Option<Vec<String>>) contract.
export async function openSession(
  parent: SessionParent,
  name: string,
  command: string[] | null = null,
): Promise<Session | null> {
  try {
    error.set(null);
    const response = await daemonRequest<Response & { session: Session }>({
      type: "open-session",
      parent,
      name: name.trim() || "shell",
      command,
    });
    activeSessionId.set(response.session.id);
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
    buffers.update((items) => {
      const next = { ...items };
      delete next[session.id];
      return next;
    });
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
  } catch {
    // Resize is best-effort; keep keystrokes flowing even if the PTY exits.
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
): Promise<void> {
  const worktreeId = get(gitWorktreeId);
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

export async function commit(subject: string, body: string | null = null): Promise<void> {
  const worktreeId = get(gitWorktreeId);
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

export async function listDraftModels(provider: DraftProvider): Promise<string[]> {
  const response = await daemonRequest<Response & { models: string[] }>({
    type: "list-draft-models",
    provider,
  });
  return response.models;
}

export async function generateCommitDraft(): Promise<CommitDraft> {
  const worktreeId = get(gitWorktreeId);
  if (!worktreeId) throw new Error("Select a git worktree first.");
  const response = await daemonRequest<Response & { draft: CommitDraft }>({
    type: "generate-commit-draft",
    worktree_id: worktreeId,
    settings: draftGenerationSettings(),
  });
  return response.draft;
}

export async function generatePullRequestDraft(base: string | null): Promise<PullRequestDraft> {
  const worktreeId = get(gitWorktreeId);
  if (!worktreeId) throw new Error("Select a git worktree first.");
  const response = await daemonRequest<Response & { draft: PullRequestDraft }>({
    type: "generate-pull-request-draft",
    worktree_id: worktreeId,
    base: base?.trim() || null,
    settings: draftGenerationSettings(),
  });
  return response.draft;
}

function draftGenerationSettings(): { provider: string; model: string | null } | null {
  const provider = get(draftProvider);
  // No explicit desktop choice → omit settings so the daemon keeps its own
  // configured provider/model default instead of being forced to "stub".
  if (!provider) return null;
  return {
    provider,
    model: get(draftModel).trim() || null,
  };
}

export async function push(): Promise<void> {
  const worktreeId = get(gitWorktreeId);
  if (!worktreeId) return;
  gitBusy.set(true);
  try {
    error.set(null);
    await daemonRequest({ type: "push", worktree_id: worktreeId });
  } catch (err) {
    error.set(toMessage(err));
  } finally {
    gitBusy.set(false);
  }
}

// Throws on failure so the dialog can surface the error inline (mirrors App.tsx).
export async function createPr(fields: PrFields): Promise<void> {
  const worktreeId = get(gitWorktreeId);
  if (!worktreeId) return;
  const response = await daemonRequest<Response & { url: string }>({
    type: "create-pull-request",
    worktree_id: worktreeId,
    title: fields.title,
    body: fields.body,
    base: fields.base,
    draft: fields.draft,
  });
  prUrl.set(response.url);
}

// ---- selection fix-up + cleanup (run once, here, as subscriptions) --------

// Fall back to the first project when nothing is selected yet.
projects.subscribe(($projects) => {
  if (!get(selectedProjectId) && $projects.length > 0) {
    selectedProjectId.set($projects[0].id);
  }
});

// Keep the selected worktree valid for the selected project: plain projects
// have none; git projects fall back to main (or the first) when the current
// selection no longer belongs to the project.
derived([selectedProject, projectWorktrees], (v) => v).subscribe(
  ([$project, $worktrees]) => {
    const selected = get(selectedWorktreeId);
    if ($project?.kind === "plain") {
      if (selected !== null) selectedWorktreeId.set(null);
      return;
    }
    if ($project && !$worktrees.some((w) => w.id === selected)) {
      selectedWorktreeId.set(
        $worktrees.find((w) => w.is_main)?.id ?? $worktrees[0]?.id ?? null,
      );
    }
  },
);

// Keep the active session within the currently visible set.
visibleSessions.subscribe(($visible) => {
  const active = get(activeSessionId);
  if (!active || !$visible.some((s) => s.id === active)) {
    activeSessionId.set($visible[0]?.id ?? null);
  }
});

// Forget agent state + running command for sessions that have closed (or were
// dropped on a reconnect), so stale labels never linger on the tree or tabs.
sessions.subscribe(($sessions) => {
  const liveIds = new Set($sessions.map((s) => s.id));
  agentStates.update((current) => {
    const next = Object.fromEntries(
      Object.entries(current).filter(([id]) => liveIds.has(id)),
    );
    return Object.keys(next).length === Object.keys(current).length ? current : next;
  });
  sessionCommands.update((current) => {
    const next = Object.fromEntries(
      Object.entries(current).filter(([id]) => liveIds.has(id)),
    );
    return Object.keys(next).length === Object.keys(current).length ? current : next;
  });
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
  stopGitStatusPolling();
  if ($id) {
    void loadGitStatus($id).catch((err) => error.set(toMessage(err)));
    startGitStatusPolling($id);
  }
});
