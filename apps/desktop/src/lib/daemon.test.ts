// Unit tests for the Daemon Status mapping (ADR 0009) and the Job store's
// StartJob -> JobCompleted resolution (ADR 0008). These are pure store/logic
// tests: the Tauri `invoke`/`listen`/`Channel` surface is mocked so the module
// loads under the node-based vitest config without a webview.

import { beforeEach, describe, expect, it, vi } from "vitest";
import { get } from "svelte/store";

// `daemon.ts` calls `invoke` (for StartJob) and imports `Channel`/`listen` at
// module load. Mock them so importing the module is side-effect-free here.
const invokeMock = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => invokeMock(...args),
  Channel: class {
    onmessage: ((msg: unknown) => void) | null = null;
  },
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async () => () => {}),
}));

import {
  addProject,
  activeSessionId,
  agentActRollupByProject,
  agentStateByWorktree,
  agentStates,
  applyDaemonStatus,
  applyHitchEvent,
  applyJobProgress,
  cancellableJobForSelectedWorktree,
  cancelJob,
  commit,
  completeJob,
  connection,
  createWorktree,
  daemonReason,
  displaySessionStates,
  diffActive,
  diffPath,
  diffText,
  dirtyWorktrees,
  daemonStatus,
  disposeDaemon,
  error,
  fetchRemote,
  generateCommitDraft,
  gitStatus,
  initDaemon,
  isJobCancellable,
  jobs,
  listDraftModels,
  loadPrStatus,
  loadProjectPrStatuses,
  prByWorktree,
  prInfo,
  prUrl,
  projects,
  refreshAll,
  push,
  reconnect,
  restartDaemon,
  runJob,
  selectedProjectId,
  selectedWorktreeId,
  sessionAgents,
  sessionCommands,
  sessionOutputActive,
  sessions,
  viewDiff,
  worktreeLineStats,
  worktrees,
} from "./daemon";
import { draftClaudePath, draftCodexPath, draftModel, draftProvider } from "./settings";

// Flush the StartJob promise chain (runJob -> daemonRequest -> invoke) so the
// pending resolver is registered before we deliver the JobCompleted event.
const flush = () => new Promise((resolve) => setTimeout(resolve, 0));

beforeEach(() => {
  disposeDaemon();
  invokeMock.mockReset();
  jobs.set({});
  error.set(null);
  daemonReason.set(null);
  daemonStatus.set("starting");
  connection.set("connecting");
  selectedWorktreeId.set(null);
  activeSessionId.set(null);
  projects.set([]);
  worktrees.set([]);
  sessions.set([]);
  agentStates.set({});
  sessionAgents.set({});
  sessionOutputActive.set({});
  dirtyWorktrees.set({});
  worktreeLineStats.set({});
  gitStatus.set(null);
  diffPath.set(null);
  prByWorktree.set({});
  prInfo.set(null);
  prUrl.set(null);
  sessionCommands.set({});
  diffText.set(null);
  diffActive.set(false);
  draftProvider.set(null);
  draftModel.set("");
  draftClaudePath.set("");
  draftCodexPath.set("");
});

describe("daemon status mapping", () => {
  it("maps the four states onto status, reason, and the derived connection", () => {
    applyDaemonStatus("starting", null);
    expect(get(daemonStatus)).toBe("starting");
    expect(get(connection)).toBe("connecting");

    applyDaemonStatus("running", null);
    expect(get(daemonStatus)).toBe("running");
    expect(get(connection)).toBe("ready");
    expect(get(error)).toBeNull();

    applyDaemonStatus("failed", "boom: store corrupt");
    expect(get(daemonStatus)).toBe("failed");
    expect(get(connection)).toBe("offline");
    expect(get(daemonReason)).toBe("boom: store corrupt");
    expect(get(error)).toBe("boom: store corrupt");

    applyDaemonStatus("unreachable", null);
    expect(get(daemonStatus)).toBe("unreachable");
    expect(get(connection)).toBe("offline");
  });


  it("fails in-flight jobs when the daemon becomes unreachable", async () => {
    invokeMock.mockResolvedValueOnce({ type: "job-started", job_id: "j-lost" });
    const pending = runJob({ type: "push", worktree_id: "w1" });
    // Let the StartJob round-trip register the pending resolver.
    await flush();
    expect(get(jobs)["j-lost"]).toBeTruthy();

    applyDaemonStatus("unreachable", "socket closed");
    await expect(pending).rejects.toThrow(/daemon restarted/);
    expect(get(jobs)).toEqual({});
  });
});

describe("agent state propagation", () => {
  it("seeds and clears per-session state and identity from session-opened replay", () => {
    applyHitchEvent({
      type: "session-opened",
      session: { id: "s1", name: "claude", parent: { kind: "project", id: "p1" }, cwd: "/repo" },
      agent: "claude-code",
      agent_state: "waiting",
      agent_detail: null,
      output_active: false,
    } as any);
    expect(get(agentStates)).toEqual({ s1: "waiting" });
    expect(get(sessionAgents)).toEqual({ s1: "claude-code" });

    // A null-state replay clears state but identity persists while the agent is
    // still announced (the replay still carries `agent`).
    applyHitchEvent({
      type: "session-opened",
      session: { id: "s1", name: "claude", parent: { kind: "project", id: "p1" }, cwd: "/repo" },
      agent: "claude-code",
      agent_state: null,
      agent_detail: null,
      output_active: false,
    } as any);
    expect(get(agentStates)).toEqual({});
    expect(get(sessionAgents)).toEqual({ s1: "claude-code" });

    agentStates.set({ s1: "error" });
    applyHitchEvent({
      type: "session-opened",
      session: { id: "s1", name: "claude", parent: { kind: "project", id: "p1" }, cwd: "/repo" },
      agent: "claude-code",
      agent_detail: null,
    } as any);
    expect(get(agentStates)).toEqual({});
  });

  it("seeds the output gate from the session-opened replay", () => {
    applyHitchEvent({
      type: "session-opened",
      session: { id: "s1", name: "claude", parent: { kind: "project", id: "p1" }, cwd: "/repo" },
      agent: "claude-code",
      agent_state: "running",
      agent_detail: null,
      output_active: true,
    } as any);
    expect(get(sessionOutputActive)).toEqual({ s1: true });
    expect(get(displaySessionStates)).toEqual({ s1: "running" });
  });

  it("records announced identity from an agent-state announce even when state is null", () => {
    sessions.set([
      { id: "s1", name: "shell", parent: { kind: "worktree", id: "w1" }, cwd: "/repo" },
    ]);
    // SessionStart announce: agent known, state still null pre-prompt. The
    // identity must be recorded NOW — it drives the Session mark before the
    // first prompt (ADR 0011 amendment). A clear is `agent: null`, not a null
    // state: the announce broadcast carries the session's current (null) state.
    applyHitchEvent({
      type: "agent-state",
      session_id: "s1",
      worktree_id: "w1",
      agent: "claude-code",
      state: null,
      detail: null,
    } as any);
    expect(get(agentStates)).toEqual({});
    expect(get(sessionAgents)).toEqual({ s1: "claude-code" });

    // A real state report keeps the identity it carries.
    applyHitchEvent({
      type: "agent-state",
      session_id: "s1",
      worktree_id: "w1",
      agent: "claude-code",
      state: "running",
      detail: null,
    } as any);
    expect(get(sessionAgents)).toEqual({ s1: "claude-code" });
  });

  it("updates and clears agent state and identity only by session id", () => {
    sessions.set([
      { id: "s1", name: "claude", parent: { kind: "worktree", id: "w1" }, cwd: "/repo" },
    ]);

    applyHitchEvent({
      type: "agent-state",
      session_id: "s1",
      worktree_id: "w1",
      agent: "claude-code",
      state: "running",
      detail: null,
    } as any);
    expect(get(agentStates)).toEqual({ s1: "running" });
    expect(get(sessionAgents)).toEqual({ s1: "claude-code" });

    applyHitchEvent({
      type: "agent-state",
      session_id: null,
      worktree_id: "w1",
      agent: "claude-code",
      state: "needs-approval",
      detail: "permission requested",
    } as any);
    expect(get(agentStates)).toEqual({ s1: "running" });

    // Exit-to-null clears both state and identity (the mark reverts to shell).
    // The clear is distinguished from an identity announce by `agent: null`,
    // never by the null state (the announce also carries a null state).
    applyHitchEvent({
      type: "agent-state",
      session_id: "s1",
      worktree_id: "w1",
      agent: null,
      state: null,
      detail: null,
    } as any);
    expect(get(agentStates)).toEqual({});
    expect(get(sessionAgents)).toEqual({});
  });

  it("gates the WORKING display state on output activity", () => {
    sessions.set([
      { id: "s1", name: "claude", parent: { kind: "worktree", id: "w1" }, cwd: "/repo" },
    ]);
    applyHitchEvent({
      type: "agent-state",
      session_id: "s1",
      worktree_id: "w1",
      agent: "claude-code",
      state: "running",
      detail: null,
    } as any);
    // running hook state with the gate closed renders idle (no WORKING word).
    expect(get(agentStates)).toEqual({ s1: "running" });
    expect(get(displaySessionStates)).toEqual({});

    // Output rising edge opens the gate → WORKING shows.
    applyHitchEvent({ type: "output-active", session_id: "s1", worktree_id: "w1", active: true } as any);
    expect(get(displaySessionStates)).toEqual({ s1: "running" });

    // Falling edge (interrupt/hang) closes it again → WORKING drops.
    applyHitchEvent({ type: "output-active", session_id: "s1", worktree_id: "w1", active: false } as any);
    expect(get(displaySessionStates)).toEqual({});
  });

  it("never gates act states or shows the unlabeled waiting state", () => {
    sessions.set([
      { id: "s1", name: "claude", parent: { kind: "worktree", id: "w1" }, cwd: "/repo" },
    ]);
    // Act states show regardless of output activity.
    agentStates.set({ s1: "needs-approval" });
    expect(get(displaySessionStates)).toEqual({ s1: "needs-approval" });
    agentStates.set({ s1: "error" });
    expect(get(displaySessionStates)).toEqual({ s1: "error" });
    // waiting renders unlabeled (omitted from the display map entirely).
    agentStates.set({ s1: "waiting" });
    expect(get(displaySessionStates)).toEqual({});
  });

  it("rolls per-session display states up to the worktree by priority", () => {
    projects.set([{ id: "p1", name: "Hitch", root: "/repo", kind: "git-backed" }]);
    worktrees.set([
      { id: "w1", project_id: "p1", path: "/repo", branch: "main", is_main: true, is_hitch_managed: false },
      {
        id: "w2",
        project_id: "p1",
        path: "/repo/.hitch/worktrees/feature",
        branch: "feature",
        is_main: false,
        is_hitch_managed: true,
      },
    ]);
    sessions.set([
      { id: "s1", name: "claude", parent: { kind: "worktree", id: "w1" }, cwd: "/repo" },
      { id: "s2", name: "codex", parent: { kind: "worktree", id: "w1" }, cwd: "/repo" },
      { id: "s3", name: "claude", parent: { kind: "worktree", id: "w2" }, cwd: "/repo/.hitch/worktrees/feature" },
    ]);
    // s1 running with its gate open; s2 waiting (unlabeled); s3 error.
    agentStates.set({ s1: "running", s2: "waiting", s3: "error" });
    sessionOutputActive.set({ s1: true });
    expect(get(agentStateByWorktree)).toEqual({ w1: "running", w2: "error" });

    agentStates.set({ s1: "needs-approval", s2: "waiting", s3: "error" });
    expect(get(agentStateByWorktree)).toEqual({ w1: "needs-approval", w2: "error" });
  });

  it("hides selected-worktree running only when the active tab is the running session", () => {
    projects.set([{ id: "p1", name: "Hitch", root: "/repo", kind: "git-backed" }]);
    worktrees.set([
      { id: "w1", project_id: "p1", path: "/repo", branch: "main", is_main: true, is_hitch_managed: false },
      {
        id: "w2",
        project_id: "p1",
        path: "/repo/.hitch/worktrees/feature",
        branch: "feature",
        is_main: false,
        is_hitch_managed: true,
      },
    ]);
    sessions.set([
      { id: "s1", name: "claude", parent: { kind: "worktree", id: "w1" }, cwd: "/repo" },
      { id: "s2", name: "shell", parent: { kind: "worktree", id: "w1" }, cwd: "/repo" },
    ]);
    agentStates.set({ s1: "running" });
    sessionOutputActive.set({ s1: true });

    selectedWorktreeId.set("w1");
    activeSessionId.set("s1");
    expect(get(agentStateByWorktree)).toEqual({});

    activeSessionId.set("s2");
    expect(get(agentStateByWorktree)).toEqual({ w1: "running" });

    diffActive.set(true);
    activeSessionId.set("s1");
    expect(get(agentStateByWorktree)).toEqual({ w1: "running" });

    diffActive.set(false);
    selectedWorktreeId.set("w2");
    expect(get(agentStateByWorktree)).toEqual({ w1: "running" });
  });

  it("rolls up a per-project act-state pill with its count, collapsing to the highest priority", () => {
    projects.set([{ id: "p1", name: "Hitch", root: "/repo", kind: "git-backed" }]);
    worktrees.set([
      { id: "w1", project_id: "p1", path: "/repo", branch: "main", is_main: true, is_hitch_managed: false },
      {
        id: "w2",
        project_id: "p1",
        path: "/repo/.hitch/worktrees/feature",
        branch: "feature",
        is_main: false,
        is_hitch_managed: true,
      },
    ]);
    sessions.set([
      { id: "s1", name: "claude", parent: { kind: "worktree", id: "w1" }, cwd: "/repo" },
      { id: "s2", name: "codex", parent: { kind: "worktree", id: "w1" }, cwd: "/repo" },
      { id: "s3", name: "claude", parent: { kind: "worktree", id: "w2" }, cwd: "/repo/.hitch/worktrees/feature" },
    ]);

    // Two act sessions (error + needs-approval) collapse to needs-approval (the
    // higher priority) with count 2; the running/waiting ones don't count.
    agentStates.set({ s1: "error", s2: "running", s3: "needs-approval" });
    expect(get(agentActRollupByProject)).toEqual({ p1: { state: "needs-approval", count: 2 } });

    // Only errors → error rollup with count 1.
    agentStates.set({ s1: "error", s2: "waiting", s3: "running" });
    expect(get(agentActRollupByProject)).toEqual({ p1: { state: "error", count: 1 } });

    // No act states → no pill (act rollup never hides behind output gating).
    agentStates.set({ s1: "running", s2: "waiting" });
    sessionOutputActive.set({});
    expect(get(agentActRollupByProject)).toEqual({});
  });

  it("counts project-parented sessions toward the project act rollup", () => {
    projects.set([{ id: "p1", name: "Plain", root: "/repo", kind: "plain" }]);
    sessions.set([
      { id: "s1", name: "claude", parent: { kind: "project", id: "p1" }, cwd: "/repo" },
    ]);
    agentStates.set({ s1: "needs-approval" });
    expect(get(agentActRollupByProject)).toEqual({ p1: { state: "needs-approval", count: 1 } });
  });

  it("drops per-session state, identity, and output gate when the session closes", () => {
    worktrees.set([
      { id: "w1", project_id: "p1", path: "/repo", branch: "main", is_main: true, is_hitch_managed: false },
    ]);
    sessions.set([
      { id: "s1", name: "claude", parent: { kind: "worktree", id: "w1" }, cwd: "/repo" },
    ]);
    agentStates.set({ s1: "running" });
    sessionAgents.set({ s1: "claude-code" });
    sessionOutputActive.set({ s1: true });

    applyHitchEvent({ type: "session-closed", session_id: "s1", exit_code: null } as any);
    expect(get(agentStates)).toEqual({});
    expect(get(sessionAgents)).toEqual({});
    expect(get(sessionOutputActive)).toEqual({});
    expect(get(agentStateByWorktree)).toEqual({});
  });
});

describe("connect snapshot refresh failures", () => {
  it.each([
    {
      name: "initial connect",
      run: () => initDaemon(),
      command: "connect_daemon",
      includeStatusProbe: true,
    },
    {
      name: "reconnect",
      run: () => reconnect(),
      command: "connect_daemon",
      includeStatusProbe: false,
    },
    {
      name: "restart",
      run: () => restartDaemon(),
      command: "restart_daemon_command",
      includeStatusProbe: false,
    },
  ])("keeps the daemon running when $name refresh fails", async ({ run, command, includeStatusProbe }) => {
    invokeMock.mockResolvedValueOnce({ type: "job-started", job_id: "j-keep" });
    const pending = runJob({ type: "push", worktree_id: "w1" });
    await flush();

    let rejected = false;
    void pending.catch(() => {
      rejected = true;
    });

    invokeMock.mockImplementation(async (invokedCommand: string, payload?: { request?: { type: string } }) => {
      if (includeStatusProbe && invokedCommand === "get_daemon_status") {
        return { status: "starting", reason: null, log_path: "/tmp/hitch-daemon.log" };
      }
      if (invokedCommand === command) {
        return undefined;
      }
      if (invokedCommand === "hitch_request" && payload?.request?.type === "list-projects") {
        throw new Error("snapshot blew up");
      }
      throw new Error(`unexpected invoke ${invokedCommand}`);
    });

    await run();
    await flush();

    expect(get(daemonStatus)).toBe("running");
    expect(get(connection)).toBe("ready");
    expect(get(error)).toBe("snapshot blew up");
    expect(get(jobs)["j-keep"]).toBeTruthy();
    expect(rejected).toBe(false);
  });


  it("hydrates daemon status from the startup snapshot before connect returns", async () => {
    let finishConnect!: () => void;
    const connectPromise = new Promise<void>((resolve) => {
      finishConnect = resolve;
    });
    invokeMock.mockImplementation(async (invokedCommand: string, payload?: { request?: { type: string } }) => {
      if (invokedCommand === "get_daemon_status") {
        return { status: "running", reason: null, log_path: "/tmp/hitch-daemon.log" };
      }
      if (invokedCommand === "connect_daemon") {
        return connectPromise;
      }
      if (invokedCommand === "hitch_request" && payload?.request?.type === "list-projects") {
        return { type: "projects", projects: [] };
      }
      if (invokedCommand === "hitch_request" && payload?.request?.type === "list-sessions") {
        return { type: "sessions", sessions: [] };
      }
      throw new Error(`unexpected invoke ${invokedCommand}`);
    });

    const init = initDaemon();
    await flush();

    expect(get(daemonStatus)).toBe("running");
    expect(get(connection)).toBe("ready");

    finishConnect();
    await init;
  });


  it("registers output channels for sessions returned by the startup snapshot", async () => {
    const session = {
      id: "s-live",
      name: "shell",
      parent: { kind: "project", id: "p1" },
      cwd: "C:/repo",
    };

    invokeMock.mockImplementation(async (invokedCommand: string, payload?: { request?: { type: string } }) => {
      if (invokedCommand === "get_daemon_status") {
        return { status: "running", reason: null, log_path: "/tmp/hitch-daemon.log" };
      }
      if (invokedCommand === "connect_daemon") {
        return undefined;
      }
      if (invokedCommand === "register_session_output") {
        return undefined;
      }
      if (invokedCommand === "hitch_request" && payload?.request?.type === "list-projects") {
        return {
          type: "projects",
          projects: [{ id: "p1", name: "Repo", root: "C:/repo", kind: "plain" }],
        };
      }
      if (invokedCommand === "hitch_request" && payload?.request?.type === "list-sessions") {
        return { type: "sessions", sessions: [session] };
      }
      throw new Error(`unexpected invoke ${invokedCommand}`);
    });

    await initDaemon();

    expect(get(sessions)).toEqual([session]);
    expect(invokeMock).toHaveBeenCalledWith(
      "register_session_output",
      expect.objectContaining({ sessionId: "s-live" }),
    );
  });
  it("selects the parent of a live snapshot session on startup", async () => {
    const session = {
      id: "s-worktree",
      name: "shell",
      parent: { kind: "worktree", id: "w2" },
      cwd: "C:/repo/feature",
    };

    invokeMock.mockImplementation(async (invokedCommand: string, payload?: { request?: { type: string } }) => {
      if (invokedCommand === "get_daemon_status") {
        return { status: "running", reason: null, log_path: "/tmp/hitch-daemon.log" };
      }
      if (invokedCommand === "connect_daemon" || invokedCommand === "register_session_output") {
        return undefined;
      }
      if (invokedCommand === "hitch_request" && payload?.request?.type === "list-projects") {
        return {
          type: "projects",
          projects: [{ id: "p1", name: "Repo", root: "C:/repo", kind: "git-backed" }],
        };
      }
      if (invokedCommand === "hitch_request" && payload?.request?.type === "list-worktrees") {
        return {
          type: "worktrees",
          worktrees: [
            {
              id: "w1",
              project_id: "p1",
              path: "C:/repo",
              branch: "main",
              is_main: true,
              is_hitch_managed: false,
            },
            {
              id: "w2",
              project_id: "p1",
              path: "C:/repo/feature",
              branch: "feature",
              is_main: false,
              is_hitch_managed: true,
            },
          ],
        };
      }
      if (invokedCommand === "hitch_request" && payload?.request?.type === "list-sessions") {
        return { type: "sessions", sessions: [session] };
      }
      if (invokedCommand === "hitch_request" && payload?.request?.type === "git-status") {
        return {
          type: "git-status",
          status: {
            worktree_id: (payload.request as unknown as { worktree_id: string }).worktree_id,
            branch: "feature",
            dirty: false,
            ahead: 0,
            behind: 0,
            additions: 0,
            deletions: 0,
            files: [],
          },
        };
      }
      throw new Error(`unexpected invoke ${invokedCommand}`);
    });

    await initDaemon();

    expect(get(selectedProjectId)).toBe("p1");
    expect(get(selectedWorktreeId)).toBe("w2");
    expect(get(activeSessionId)).toBe("s-worktree");
  });

  it("keeps the current selection on reconnect when it has no live session", async () => {
    // The user is looking at worktree w2, which has no live session; the only
    // live session belongs to a different worktree (w1). The reconnect snapshot
    // must not yank the selection over to w1.
    //
    // Seed the project/worktree stores the way a live window already holds them
    // before reconnect; otherwise the selection-fixup subscription transiently
    // clears `selectedWorktreeId` when `refreshAll` re-sets `projects` ahead of
    // `worktrees`, which would mask the behavior under test.
    const liveWorktrees = [
      {
        id: "w1",
        project_id: "p1",
        path: "C:/repo",
        branch: "main",
        is_main: true,
        is_hitch_managed: false,
      },
      {
        id: "w2",
        project_id: "p1",
        path: "C:/repo/feature",
        branch: "feature",
        is_main: false,
        is_hitch_managed: true,
      },
    ];
    projects.set([{ id: "p1", name: "Repo", root: "C:/repo", kind: "git-backed" } as never]);
    worktrees.set(liveWorktrees as never);
    selectedProjectId.set("p1");
    selectedWorktreeId.set("w2");
    activeSessionId.set(null);

    const session = {
      id: "s-on-w1",
      name: "shell",
      parent: { kind: "worktree", id: "w1" },
      cwd: "C:/repo",
    };

    invokeMock.mockImplementation(async (invokedCommand: string, payload?: { request?: { type: string } }) => {
      if (invokedCommand === "connect_daemon" || invokedCommand === "register_session_output") {
        return undefined;
      }
      if (invokedCommand === "hitch_request" && payload?.request?.type === "list-projects") {
        return {
          type: "projects",
          projects: [{ id: "p1", name: "Repo", root: "C:/repo", kind: "git-backed" }],
        };
      }
      if (invokedCommand === "hitch_request" && payload?.request?.type === "list-worktrees") {
        return { type: "worktrees", worktrees: liveWorktrees };
      }
      if (invokedCommand === "hitch_request" && payload?.request?.type === "list-sessions") {
        return { type: "sessions", sessions: [session] };
      }
      if (invokedCommand === "hitch_request" && payload?.request?.type === "git-status") {
        return {
          type: "git-status",
          status: {
            worktree_id: (payload.request as unknown as { worktree_id: string }).worktree_id,
            branch: "feature",
            dirty: false,
            ahead: 0,
            behind: 0,
            additions: 0,
            deletions: 0,
            files: [],
          },
        };
      }
      throw new Error(`unexpected invoke ${invokedCommand}`);
    });

    await reconnect();

    expect(get(selectedProjectId)).toBe("p1");
    expect(get(selectedWorktreeId)).toBe("w2");
    expect(get(activeSessionId)).toBeNull();
  });


  it("does not reset an output channel already registered by a session-opened replay", async () => {
    const session = {
      id: "s-replayed",
      name: "shell",
      parent: { kind: "project", id: "p1" },
      cwd: "C:/repo",
    };
    invokeMock.mockResolvedValue(undefined);
    applyHitchEvent({ type: "session-opened", session });
    invokeMock.mockClear();
    invokeMock.mockImplementation(async (invokedCommand: string, payload?: { request?: { type: string } }) => {
      if (invokedCommand === "hitch_request" && payload?.request?.type === "list-projects") {
        return {
          type: "projects",
          projects: [{ id: "p1", name: "Repo", root: "C:/repo", kind: "plain" }],
        };
      }
      if (invokedCommand === "hitch_request" && payload?.request?.type === "list-sessions") {
        return { type: "sessions", sessions: [session] };
      }
      throw new Error(`unexpected invoke ${invokedCommand}`);
    });

    await refreshAll();

    expect(invokeMock).not.toHaveBeenCalledWith(
      "register_session_output",
      expect.objectContaining({ sessionId: "s-replayed" }),
    );
  });

  it("marks the daemon failed when reconnect cannot reach it", async () => {
    invokeMock.mockRejectedValueOnce(new Error("connect refused"));

    await reconnect();

    expect(get(daemonStatus)).toBe("failed");
    expect(get(connection)).toBe("offline");
    expect(get(error)).toBe("connect refused");
  });
});

describe("job store: StartJob -> JobCompleted", () => {
  it("resolves the caller's promise with the wrapped response", async () => {
    invokeMock.mockResolvedValueOnce({ type: "job-started", job_id: "j1" });
    const promise = runJob<{ type: string; url: string }>(
      {
        type: "create-pull-request",
        worktree_id: "w1",
        title: "Title",
        body: null,
        base: null,
        draft: false,
      },
      "create-pr",
    );
    await flush();

    expect(get(jobs)["j1"]).toMatchObject({
      status: "running",
      kind: "create-pr",
      worktreeId: "w1",
    });

    // The wrapped response arrives inside JobCompleted.
    completeJob("j1", { type: "pull-request-created", url: "https://x/pull/1" });
    await expect(promise).resolves.toMatchObject({ url: "https://x/pull/1" });
    // The job is cleared from the live store once complete.
    expect(get(jobs)["j1"]).toBeUndefined();
  });

  it("rejects the caller's promise when the job completes with an error", async () => {
    invokeMock.mockResolvedValueOnce({ type: "job-started", job_id: "j2" });
    const promise = runJob({ type: "push", worktree_id: "w1" });
    await flush();

    completeJob("j2", { type: "error", error: { message: "remote rejected" } });
    await expect(promise).rejects.toThrow("remote rejected");
  });

  it("routes fetch through StartJob for the requested worktree", async () => {
    invokeMock.mockResolvedValueOnce({ type: "job-started", job_id: "j-fetch" });

    const promise = fetchRemote("w-fetch");
    await flush();

    expect(invokeMock).toHaveBeenCalledWith("hitch_request", {
      request: {
        type: "start-job",
        request: { type: "fetch", worktree_id: "w-fetch" },
      },
    });
    expect(get(jobs)["j-fetch"]).toMatchObject({
      status: "running",
      kind: "fetch",
      worktreeId: "w-fetch",
    });

    completeJob("j-fetch", { type: "ack" });
    await expect(promise).resolves.toBeUndefined();
  });

  it("passes configured draft provider executable paths to model discovery", async () => {
    invokeMock.mockResolvedValueOnce({ type: "job-started", job_id: "j-models" });

    const promise = listDraftModels("codex", { codexPath: "C:\\Program Files\\Codex\\codex.exe" });
    await flush();

    expect(invokeMock).toHaveBeenCalledWith("hitch_request", {
      request: {
        type: "start-job",
        request: {
          type: "list-draft-models",
          provider: "codex",
          settings: {
            provider: "codex",
            model: null,
            claude_path: null,
            codex_path: "C:\\Program Files\\Codex\\codex.exe",
          },
        },
      },
    });

    completeJob("j-models", { type: "draft-models", provider: "codex", models: ["gpt-5-codex"] });
    await expect(promise).resolves.toEqual(["gpt-5-codex"]);
  });

  it("passes saved draft provider executable paths to generation jobs", async () => {
    draftProvider.set("claude");
    draftModel.set("sonnet");
    draftClaudePath.set("C:\\Program Files\\Claude\\claude.exe");
    draftCodexPath.set("C:\\Program Files\\Codex\\codex.exe");
    invokeMock.mockResolvedValueOnce({ type: "job-started", job_id: "j-draft" });

    const promise = generateCommitDraft("w-draft");
    await flush();

    expect(invokeMock).toHaveBeenCalledWith("hitch_request", {
      request: {
        type: "start-job",
        request: {
          type: "generate-commit-draft",
          worktree_id: "w-draft",
          settings: {
            provider: "claude",
            model: "sonnet",
            claude_path: "C:\\Program Files\\Claude\\claude.exe",
            codex_path: "C:\\Program Files\\Codex\\codex.exe",
          },
        },
      },
    });

    completeJob("j-draft", {
      type: "commit-draft",
      draft: { subject: "feat: generated", body: "- Generated" },
    });
    await expect(promise).resolves.toEqual({ subject: "feat: generated", body: "- Generated" });
  });

  it("rebuilds a replayed running job so its later completion is applied", () => {
    applyJobProgress("j-reattach", "running", "Pushing…", "push");

    expect(get(jobs)["j-reattach"]).toMatchObject({
      status: "running",
      message: "Pushing…",
      kind: "push",
      worktreeId: null,
    });

    completeJob("j-reattach", { type: "ack" });
    expect(get(jobs)["j-reattach"]).toBeUndefined();
  });

  it("resolves completions that arrive before the StartJob continuation resumes", async () => {
    let startJob!: (response: { type: string; job_id: string }) => void;
    invokeMock.mockReturnValueOnce(
      new Promise((resolve) => {
        startJob = resolve;
      }),
    );

    const promise = runJob<{ type: string; url: string }>(
      {
        type: "create-pull-request",
        worktree_id: "w1",
        title: "Fast title",
        body: null,
        base: null,
        draft: false,
      },
      "create-pr",
    );
    startJob({ type: "job-started", job_id: "j-fast" });
    completeJob("j-fast", {
      type: "pull-request-created",
      url: "https://x/pull/fast",
    });

    await expect(promise).resolves.toMatchObject({ url: "https://x/pull/fast" });
    expect(get(jobs)["j-fast"]).toBeUndefined();
  });

  it("routes create-worktree through StartJob and resolves the created worktree", async () => {
    invokeMock.mockImplementation(async (_command: string, { request }: { request: { type: string } }) => {
      switch (request.type) {
        case "start-job":
          return { type: "job-started", job_id: "j-worktree" };
        case "list-projects":
          return { type: "projects", projects: [] };
        case "list-sessions":
          return { type: "sessions", sessions: [] };
        default:
          throw new Error(`unexpected request ${request.type}`);
      }
    });

    const promise = createWorktree("p1", "feat/demo", "main", "new-branch");
    await flush();

    expect(invokeMock).toHaveBeenNthCalledWith(1, "hitch_request", {
      request: {
        type: "start-job",
        request: {
          type: "create-worktree",
          project_id: "p1",
          branch: "feat/demo",
          base: "main",
          mode: "new-branch",
        },
      },
    });

    completeJob("j-worktree", {
      type: "worktrees",
      worktrees: [
        {
          id: "w-new",
          project_id: "p1",
          path: "/tmp/w-new",
          branch: "feat/demo",
          is_main: false,
          is_hitch_managed: true,
        },
      ],
    });

    await expect(promise).resolves.toMatchObject({ id: "w-new", branch: "feat/demo" });
    expect(get(selectedWorktreeId)).toBe("w-new");
  });

  it("tracks the active worktree's cancellable job", () => {
    jobs.set({
      foreign: {
        id: "foreign",
        status: "running",
        message: null,
        kind: "commit-draft",
        worktreeId: "w-foreign",
      },
      local: {
        id: "local",
        status: "queued",
        message: null,
        kind: "pr-draft",
        worktreeId: "w-local",
      },
      push: {
        id: "push",
        status: "running",
        message: null,
        kind: "push",
        worktreeId: "w-local",
      },
      prStatus: {
        id: "prStatus",
        status: "running",
        message: null,
        kind: "pr-status",
        worktreeId: "w-pr-status",
      },
    });

    selectedWorktreeId.set("w-local");
    expect(get(cancellableJobForSelectedWorktree)?.id).toBe("local");

    selectedWorktreeId.set("w-foreign");
    expect(get(cancellableJobForSelectedWorktree)?.id).toBe("foreign");

    selectedWorktreeId.set("w-pr-status");
    expect(get(cancellableJobForSelectedWorktree)).toBeNull();

    selectedWorktreeId.set("w-missing");
    expect(get(cancellableJobForSelectedWorktree)).toBeNull();
  });

  it("reflects progress transitions, including cancellation", () => {
    applyJobProgress("j3", "running", "Pushing…");
    expect(get(jobs)["j3"]).toMatchObject({
      status: "running",
      message: "Pushing…",
      worktreeId: null,
    });

    applyJobProgress("j3", "cancelled", null);
    expect(get(jobs)["j3"].status).toBe("cancelled");
  });

  it("keeps the local job kind when later progress omits it", async () => {
    invokeMock.mockResolvedValueOnce({ type: "job-started", job_id: "j-local" });
    void runJob({ type: "push", worktree_id: "w1" }, "push");
    await flush();

    applyJobProgress("j-local", "running", "Still pushing…");
    expect(get(jobs)["j-local"]).toMatchObject({
      status: "running",
      message: "Still pushing…",
      kind: "push",
      worktreeId: "w1",
    });
  });

  it("sends a cancel-job request for the given id", async () => {
    invokeMock.mockResolvedValueOnce({ type: "ack" });
    await cancelJob("j4");
    expect(invokeMock).toHaveBeenCalledWith("hitch_request", {
      request: { type: "cancel-job", job_id: "j4" },
    });
  });

  it("marks foreground cancellable job kinds as cancellable", () => {
    expect(
      isJobCancellable({
        id: "j-clone",
        status: "running",
        message: null,
        kind: "clone",
        worktreeId: null,
      }),
    ).toBe(true);
    expect(
      isJobCancellable({
        id: "j-worktree",
        status: "queued",
        message: null,
        kind: "create-worktree",
        worktreeId: "w1",
      }),
    ).toBe(true);
    expect(
      isJobCancellable({
        id: "j-push",
        status: "running",
        message: null,
        kind: "push",
        worktreeId: "w1",
      }),
    ).toBe(true);
    expect(
      isJobCancellable({
        id: "j-fetch",
        status: "running",
        message: null,
        kind: "fetch",
        worktreeId: "w1",
      }),
    ).toBe(true);
    expect(
      isJobCancellable({
        id: "j-pull",
        status: "running",
        message: null,
        kind: "pull",
        worktreeId: "w1",
      }),
    ).toBe(true);
    expect(
      isJobCancellable({
        id: "j-pr",
        status: "running",
        message: null,
        kind: "create-pr",
        worktreeId: "w1",
      }),
    ).toBe(true);
    expect(
      isJobCancellable({
        id: "j-pr-status",
        status: "running",
        message: null,
        kind: "pr-status",
        worktreeId: "w1",
      }),
    ).toBe(false);
    expect(
      isJobCancellable({
        id: "j6b",
        status: "queued",
        message: null,
        kind: "draft-models",
        worktreeId: "w1",
      }),
    ).toBe(true);
    expect(
      isJobCancellable({
        id: "j5",
        status: "running",
        message: null,
        kind: "commit-draft",
        worktreeId: "w1",
      }),
    ).toBe(true);
    expect(
      isJobCancellable({
        id: "j6",
        status: "running",
        message: null,
        kind: "pr-draft",
        worktreeId: "w1",
      }),
    ).toBe(true);
    expect(
      isJobCancellable({
        id: "j8",
        status: "running",
        message: null,
        kind: null,
        worktreeId: "w1",
      }),
    ).toBe(false);
  });

  it("does not keep early completions for jobs started by another window", async () => {
    completeJob("foreign", { type: "ack" });

    invokeMock.mockResolvedValueOnce({ type: "job-started", job_id: "foreign" });
    let settled = false;
    const promise = runJob({ type: "push", worktree_id: "w1" }).then((response) => {
      settled = true;
      return response;
    });
    await flush();
    await flush();

    expect(settled).toBe(false);

    completeJob("foreign", { type: "ack" });
    await expect(promise).resolves.toMatchObject({ type: "ack" });
    expect(settled).toBe(true);
  });

});
describe("Windows project paths", () => {
  const projectRoot = String.raw`C:\Users\Ada Lovelace\Repo With Spaces`;
  const filePath = String.raw`src\folder with spaces\file name.ts`;
  const worktreePath = String.raw`C:\Users\Ada Lovelace\Repo With Spaces`;
  const project = {
    id: "project-win",
    name: "Repo With Spaces",
    root: projectRoot,
    kind: "git-backed",
  } as const;
  const worktree = {
    id: "worktree-win",
    project_id: "project-win",
    path: worktreePath,
    branch: "main",
    is_main: true,
    is_hitch_managed: false,
  } as const;

  function windowsStatus(additions = 12, deletions = 5) {
    return {
      worktree_id: worktree.id,
      branch: "main",
      dirty: true,
      ahead: 0,
      behind: 0,
      additions,
      deletions,
      files: [{ path: filePath, status: "modified" as const, staged: false }],
    };
  }

  it("adds and refreshes a git-backed project without changing a Windows root with spaces", async () => {
    const requests: unknown[] = [];
    invokeMock.mockImplementation(
      async (_command: string, { request }: { request: { type: string; project_id?: string } }) => {
        requests.push(request);
        if (request.type === "add-project") return { type: "ack" };
        if (request.type === "list-projects") return { type: "list-projects", projects: [project] };
        if (request.type === "list-worktrees") {
          expect(request.project_id).toBe(project.id);
          return { type: "list-worktrees", worktrees: [worktree] };
        }
        if (request.type === "list-sessions") return { type: "list-sessions", sessions: [] };
        if (request.type === "git-status") return { type: "git-status", status: windowsStatus() };
        if (request.type === "start-job") return { type: "job-started", job_id: "j-pr" };
        throw new Error(`unexpected request ${request.type}`);
      },
    );

    await addProject(projectRoot);

    expect(requests[0]).toEqual({ type: "add-project", root: projectRoot });
    expect(get(projects)[0]?.root).toBe(projectRoot);
    expect(get(worktrees)[0]?.path).toBe(worktreePath);
    expect(get(dirtyWorktrees)[worktree.id]).toBe(true);
    expect(get(worktreeLineStats)[worktree.id]).toEqual({ additions: 12, deletions: 5 });

    projects.set([]);
    worktrees.set([]);
    dirtyWorktrees.set({});
    worktreeLineStats.set({});

    await refreshAll();

    expect(get(projects)[0]?.root).toBe(projectRoot);
    expect(get(worktrees)[0]?.path).toBe(worktreePath);
  });

  it("refreshes dirty state and line stats from a worktree-dirty event for a Windows worktree path", async () => {
    projects.set([project]);
    worktrees.set([worktree]);
    selectedWorktreeId.set(worktree.id);
    invokeMock.mockImplementation(
      async (_command: string, { request }: { request: { type: string; worktree_id?: string } }) => {
        expect(request).toEqual({ type: "git-status", worktree_id: worktree.id });
        return { type: "git-status", status: windowsStatus(7, 3) };
      },
    );

    applyHitchEvent({ type: "worktree-dirty", worktree_id: worktree.id, dirty: true });
    await flush();

    expect(get(dirtyWorktrees)[worktree.id]).toBe(true);
    expect(get(worktreeLineStats)[worktree.id]).toEqual({ additions: 7, deletions: 3 });
    expect(get(gitStatus)?.files[0]?.path).toBe(filePath);
  });

  it("sends the exact Windows file path when requesting a diff", async () => {
    const requests: unknown[] = [];
    projects.set([project]);
    worktrees.set([worktree]);
    selectedWorktreeId.set(worktree.id);
    invokeMock.mockImplementation(
      async (_command: string, { request }: { request: { type: string } }) => {
        requests.push(request);
        if (request.type === "git-diff") return { type: "git-diff", diff: { diff: "diff --git" } };
        throw new Error(`unexpected request ${request.type}`);
      },
    );

    await viewDiff(filePath);

    expect(requests).toEqual([{ type: "git-diff", worktree_id: worktree.id, path: filePath }]);
    expect(get(diffPath)).toBe(filePath);
    expect(get(diffText)).toBe("diff --git");
  });

  it("creates a Windows managed worktree through a job and reflects the daemon worktree event", async () => {
    const managedBranch = "feature/windows/worktree-safe-dir";
    const managedWorktree = {
      id: "worktree-managed-win",
      project_id: project.id,
      path: String.raw`C:\Users\Ada Lovelace\AppData\Local\Hitch\worktrees\repo-with-spaces\feature-windows-worktree-safe-dir`,
      branch: managedBranch,
      is_main: false,
      is_hitch_managed: true,
    } as const;
    const requests: unknown[] = [];
    invokeMock.mockImplementation(
      async (_command: string, { request }: { request: { type: string; request?: unknown } }) => {
        requests.push(request);
        if (request.type === "start-job") return { type: "job-started", job_id: "j-create-win" };
        throw new Error(`unexpected request ${request.type}`);
      },
    );

    projects.set([project]);
    worktrees.set([worktree]);

    const promise = createWorktree(project.id, ` ${managedBranch} `, "main", "new-branch");
    await flush();

    expect(requests).toEqual([
      {
        type: "start-job",
        request: {
          type: "create-worktree",
          project_id: project.id,
          branch: managedBranch,
          base: "main",
          mode: "new-branch",
        },
      },
    ]);

    applyHitchEvent({ type: "worktree-updated", worktree: managedWorktree });
    expect(get(worktrees).map((item) => item.path)).toContain(managedWorktree.path);

    completeJob("j-create-win", { type: "worktrees", worktrees: [managedWorktree] });
    await expect(promise).resolves.toMatchObject({
      id: managedWorktree.id,
      branch: managedBranch,
      path: managedWorktree.path,
    });
    expect(get(selectedWorktreeId)).toBe(managedWorktree.id);
    expect(get(worktrees).filter((item) => item.id === managedWorktree.id)).toHaveLength(1);
  });

  it("removes a Windows managed worktree event from the tree and clears worktree-scoped frontend state", () => {
    const removed = {
      id: "worktree-remove-win",
      project_id: project.id,
      path: String.raw`C:\Users\Ada Lovelace\AppData\Local\Hitch\worktrees\repo-with-spaces\bugfix-win-safe-dir`,
      branch: "bugfix/windows/worktree-safe-dir",
      is_main: false,
      is_hitch_managed: true,
    } as const;
    projects.set([project]);
    worktrees.set([worktree, removed]);
    sessions.set([
      { id: "session-remove", name: "claude", parent: { kind: "worktree", id: removed.id }, cwd: removed.path },
      { id: "session-keep", name: "shell", parent: { kind: "worktree", id: worktree.id }, cwd: worktree.path },
    ]);
    selectedWorktreeId.set(removed.id);
    activeSessionId.set("session-remove");
    agentStates.set({ "session-remove": "waiting", "session-keep": "running" });
    sessionAgents.set({ "session-remove": "claude-code" });
    sessionOutputActive.set({ "session-remove": true, "session-keep": true });
    sessionCommands.set({ "session-remove": "claude", "session-keep": "pwsh" });
    dirtyWorktrees.set({ [removed.id]: true, [worktree.id]: false });
    worktreeLineStats.set({
      [removed.id]: { additions: 9, deletions: 4 },
      [worktree.id]: { additions: 0, deletions: 0 },
    });
    gitStatus.set({
      worktree_id: removed.id,
      branch: removed.branch,
      dirty: true,
      ahead: 0,
      behind: 0,
      additions: 9,
      deletions: 4,
      files: [{ path: filePath, status: "modified", staged: false }],
    });
    diffPath.set(filePath);
    diffText.set("diff --git a/file b/file");
    diffActive.set(true);
    prByWorktree.set({
      [removed.id]: { number: 7, url: "https://example.test/pr/7", state: "OPEN", draft: false },
    });
    prInfo.set({ number: 7, url: "https://example.test/pr/7", state: "OPEN", draft: false });
    prUrl.set("https://example.test/pr/7");

    applyHitchEvent({ type: "worktree-removed", worktree_id: removed.id });

    expect(get(worktrees).map((item) => item.id)).toEqual([worktree.id]);
    expect(get(sessions).map((item) => item.id)).toEqual(["session-keep"]);
    expect(get(selectedWorktreeId)).toBeNull();
    expect(get(activeSessionId)).toBeNull();
    expect(get(gitStatus)).toBeNull();
    expect(get(diffPath)).toBeNull();
    expect(get(diffText)).toBeNull();
    expect(get(diffActive)).toBe(false);
    expect(get(dirtyWorktrees)).toEqual({ [worktree.id]: false });
    expect(get(worktreeLineStats)).toEqual({ [worktree.id]: { additions: 0, deletions: 0 } });
    expect(get(prByWorktree)).toEqual({});
    expect(get(prInfo)).toBeNull();
    expect(get(prUrl)).toBeNull();
    expect(get(agentStates)).toEqual({ "session-keep": "running" });
    expect(get(sessionAgents)).toEqual({});
    expect(get(sessionOutputActive)).toEqual({ "session-keep": true });
    expect(get(sessionCommands)).toEqual({ "session-keep": "pwsh" });
    expect(get(agentStateByWorktree)).toEqual({ [worktree.id]: "running" });
  });
});


describe("worktree-scoped git actions", () => {
  it("keeps a commit-then-push sequence on the triggering worktree after selection changes", async () => {
    const requests: unknown[] = [];
    invokeMock.mockImplementation(
      async (_command: string, { request }: { request: { type: string; request?: { type: string } } }) => {
        requests.push(request);
        if (request.type === "commit") {
          selectedWorktreeId.set("w-active");
          return { type: "ack" };
        }
        if (request.type === "git-status") {
          return {
            type: "git-status",
            status: {
              worktree_id: "w-trigger",
              branch: "feature",
              dirty: false,
              ahead: 1,
              behind: 0,
              additions: 0,
              deletions: 0,
              files: [],
            },
          };
        }
        if (request.type === "start-job" && request.request?.type === "push") {
          return { type: "job-started", job_id: "j-trigger-push" };
        }
        throw new Error(`unexpected request ${request.type}`);
      },
    );

    selectedWorktreeId.set("w-trigger");
    await commit("Lock target", null, "w-trigger");
    expect(get(selectedWorktreeId)).toBe("w-active");

    const pushPromise = push("w-trigger");
    await flush();

    expect(requests).toContainEqual({
      type: "start-job",
      request: { type: "push", worktree_id: "w-trigger" },
    });

    completeJob("j-trigger-push", { type: "ack" });
    await expect(pushPromise).resolves.toBeUndefined();
  });
});

describe("PR status freshness across batched and per-worktree lookups", () => {
  it("does not let an older project-wide batch clobber a fresher in-flight per-worktree lookup", async () => {
    // Regression: a project-wide `loadProjectPrStatuses` that *started* before a
    // fresher per-worktree `loadPrStatus` could resolve afterwards and overwrite
    // the worktree's chip with stale data — and if the fresher lookup then failed,
    // the chip stayed wrong until the next poll. The fix stamps an in-flight
    // (started) seq per worktree so the older batch is rejected.
    prByWorktree.set({});
    prInfo.set(null);
    selectedWorktreeId.set("wt-1");

    invokeMock.mockImplementation(
      async (_command: string, { request }: { request: { request: { type: string } } }) => {
        const type = request.request.type;
        if (type === "project-pr-statuses") return { type: "job-started", job_id: "j-batch" };
        if (type === "pr-status") return { type: "job-started", job_id: "j-pw" };
        throw new Error(`unexpected request ${type}`);
      },
    );

    // Batch starts first (older seq); the fresher per-worktree lookup for the same
    // worktree starts while the batch is still in flight (newer seq).
    const batch = loadProjectPrStatuses("proj-1", { force: true });
    const perWorktree = loadPrStatus("wt-1");
    await flush();

    // The stale batch resolves first, trying to write an outdated PR for wt-1.
    completeJob("j-batch", {
      type: "project-pr-statuses",
      statuses: [
        { worktree_id: "wt-1", pr: { number: 1, url: "stale", state: "OPEN", draft: false } },
      ],
    });
    await batch;

    // The fresher per-worktree lookup then fails; selected state and chip map must
    // both reflect the freshest failed lookup.
    completeJob("j-pw", { type: "error", error: { message: "boom" } });
    await perWorktree;

    expect(get(prByWorktree)["wt-1"]).toBeNull();
    expect(get(prInfo)).toBeNull();
  });

  it("does not let a slow failed lookup clear a newer per-worktree success", async () => {
    selectedWorktreeId.set("wt-1");
    prByWorktree.set({
      "wt-1": { number: 1, url: "old", state: "OPEN", draft: false },
    });
    prInfo.set({ number: 1, url: "old", state: "OPEN", draft: false });
    let prJobCount = 0;

    invokeMock.mockImplementation(
      async (_command: string, { request }: { request: { request: { type: string } } }) => {
        const type = request.request.type;
        if (type !== "pr-status") throw new Error(`unexpected request ${type}`);
        return { type: "job-started", job_id: `j-pr-${++prJobCount}` };
      },
    );

    const slowFailure = loadPrStatus("wt-1");
    const newerSuccess = loadPrStatus("wt-1");
    await flush();

    const freshPr = { number: 2, url: "fresh", state: "OPEN", draft: false } as const;
    completeJob("j-pr-2", { type: "pr-status", pr: freshPr });
    await newerSuccess;

    completeJob("j-pr-1", { type: "error", error: { message: "boom" } });
    await slowFailure;

    expect(get(prByWorktree)["wt-1"]).toEqual(freshPr);
    expect(get(prInfo)).toEqual(freshPr);
  });
});
