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
  activeSessionId,
  agentStateByProject,
  agentStateByWorktree,
  agentStates,
  applyDaemonStatus,
  applyHitchEvent,
  applyJobProgress,
  cancellableJobForSelectedWorktree,
  cancelJob,
  completeJob,
  connection,
  createWorktree,
  daemonReason,
  dismissedWorktreeAgentStates,
  diffActive,
  dismissedSessionAgentStates,
  daemonStatus,
  disposeDaemon,
  error,
  initDaemon,
  isJobCancellable,
  jobs,
  loadPrStatus,
  loadProjectPrStatuses,
  prByWorktree,
  prInfo,
  projects,
  reconnect,
  restartDaemon,
  runJob,
  selectedWorktreeId,
  sessions,
  worktrees,
  visibleAgentStates,
} from "./daemon";

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
  dismissedSessionAgentStates.set({});
  dismissedWorktreeAgentStates.set({});
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
  it("seeds and clears per-session state from session-opened replay", () => {
    applyHitchEvent({
      type: "session-opened",
      session: { id: "s1", name: "claude", parent: { kind: "project", id: "p1" }, cwd: "/repo" },
      agent: "claude-code",
      agent_state: "waiting",
      agent_detail: null,
    } as any);
    expect(get(agentStates)).toEqual({ s1: "waiting" });

    applyHitchEvent({
      type: "session-opened",
      session: { id: "s1", name: "claude", parent: { kind: "project", id: "p1" }, cwd: "/repo" },
      agent: "claude-code",
      agent_state: null,
      agent_detail: null,
    } as any);
    expect(get(agentStates)).toEqual({});

    agentStates.set({ s1: "error" });
    applyHitchEvent({
      type: "session-opened",
      session: { id: "s1", name: "claude", parent: { kind: "project", id: "p1" }, cwd: "/repo" },
      agent: "claude-code",
      agent_detail: null,
    } as any);
    expect(get(agentStates)).toEqual({});
  });

  it("updates and clears agent state only by session id", () => {
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

    applyHitchEvent({
      type: "agent-state",
      session_id: null,
      worktree_id: "w1",
      agent: "claude-code",
      state: "needs-approval",
      detail: "permission requested",
    } as any);
    expect(get(agentStates)).toEqual({ s1: "running" });

    applyHitchEvent({
      type: "agent-state",
      session_id: "s1",
      worktree_id: "w1",
      agent: "claude-code",
      state: null,
      detail: null,
    } as any);
    expect(get(agentStates)).toEqual({});
  });

  it("rolls per-session states up to worktree and project by priority", () => {
    projects.set([{ id: "p1", name: "Hitch", root: "/repo", kind: "git-backed" }]);
    worktrees.set([
      {
        id: "w1",
        project_id: "p1",
        path: "/repo",
        branch: "main",
        is_main: true,
        is_hitch_managed: false,
      },
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
      {
        id: "s3",
        name: "claude",
        parent: { kind: "worktree", id: "w2" },
        cwd: "/repo/.hitch/worktrees/feature",
      },
    ]);
    agentStates.set({ s1: "running", s2: "waiting", s3: "error" });
    expect(get(agentStateByWorktree)).toEqual({ w1: "waiting", w2: "error" });
    expect(get(agentStateByProject)).toEqual({ p1: "error" });

    agentStates.set({ s1: "needs-approval", s2: "waiting", s3: "error" });
    expect(get(agentStateByWorktree)).toEqual({ w1: "needs-approval", w2: "error" });
    expect(get(agentStateByProject)).toEqual({ p1: "needs-approval" });
  });

  it("dismisses waiting and error worktree rollups when seen, but not needs-approval", () => {
    projects.set([{ id: "p1", name: "Hitch", root: "/repo", kind: "git-backed" }]);
    worktrees.set([
      {
        id: "w1",
        project_id: "p1",
        path: "/repo",
        branch: "main",
        is_main: true,
        is_hitch_managed: false,
      },
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
    ]);
    agentStates.set({ s1: "waiting" });
    expect(get(agentStateByWorktree)).toEqual({ w1: "waiting" });

    selectedWorktreeId.set("w1");
    expect(get(dismissedWorktreeAgentStates)).toEqual({ w1: "waiting" });
    selectedWorktreeId.set("w2");
    expect(get(agentStateByWorktree)).toEqual({});
    expect(get(agentStateByProject)).toEqual({});

    applyHitchEvent({
      type: "agent-state",
      session_id: "s1",
      worktree_id: "w1",
      agent: "claude-code",
      state: "error",
      detail: null,
    } as any);
    expect(get(agentStateByWorktree)).toEqual({ w1: "error" });
    selectedWorktreeId.set("w1");
    selectedWorktreeId.set("w2");
    expect(get(agentStateByWorktree)).toEqual({});

    applyHitchEvent({
      type: "agent-state",
      session_id: "s1",
      worktree_id: "w1",
      agent: "claude-code",
      state: "needs-approval",
      detail: null,
    } as any);
    expect(get(agentStateByWorktree)).toEqual({ w1: "needs-approval" });
    selectedWorktreeId.set("w1");
    expect(get(dismissedWorktreeAgentStates)).toEqual({});
    expect(get(agentStateByWorktree)).toEqual({ w1: "needs-approval" });
    selectedWorktreeId.set("w2");
    expect(get(agentStateByWorktree)).toEqual({ w1: "needs-approval" });
  });

  it("hides selected-worktree running only when the active tab is the running session", () => {
    projects.set([{ id: "p1", name: "Hitch", root: "/repo", kind: "git-backed" }]);
    worktrees.set([
      {
        id: "w1",
        project_id: "p1",
        path: "/repo",
        branch: "main",
        is_main: true,
        is_hitch_managed: false,
      },
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

    selectedWorktreeId.set("w1");
    activeSessionId.set("s1");
    expect(get(dismissedWorktreeAgentStates)).toEqual({});
    expect(get(agentStateByWorktree)).toEqual({});

    activeSessionId.set("s2");
    expect(get(agentStateByWorktree)).toEqual({ w1: "running" });

    diffActive.set(true);
    activeSessionId.set("s1");
    expect(get(agentStateByWorktree)).toEqual({ w1: "running" });

    diffActive.set(false);
    selectedWorktreeId.set("w2");
    expect(get(agentStateByWorktree)).toEqual({ w1: "running" });
    expect(get(agentStateByProject)).toEqual({ p1: "running" });
  });

  it("keeps waiting and error tab status briefly visible when the user visits that tab", () => {
    vi.useFakeTimers();
    try {
      sessions.set([
        { id: "s1", name: "codex", parent: { kind: "project", id: "p1" }, cwd: "/repo" },
        { id: "s2", name: "shell", parent: { kind: "project", id: "p1" }, cwd: "/repo" },
      ]);
      activeSessionId.set("s2");
      applyHitchEvent({
        type: "agent-state",
        session_id: "s1",
        worktree_id: null,
        agent: "codex",
        state: "waiting",
        detail: null,
      } as any);
      expect(get(visibleAgentStates)).toEqual({ s1: "waiting" });

      activeSessionId.set("s1");
      expect(get(visibleAgentStates)).toEqual({ s1: "waiting" });
      vi.advanceTimersByTime(2_499);
      expect(get(visibleAgentStates)).toEqual({ s1: "waiting" });
      vi.advanceTimersByTime(1);
      expect(get(dismissedSessionAgentStates)).toEqual({ s1: "waiting" });
      expect(get(visibleAgentStates)).toEqual({});

      activeSessionId.set("s2");
      applyHitchEvent({
        type: "agent-state",
        session_id: "s1",
        worktree_id: null,
        agent: "codex",
        state: "error",
        detail: null,
      } as any);
      expect(get(visibleAgentStates)).toEqual({ s1: "error" });
      activeSessionId.set("s1");
      expect(get(visibleAgentStates)).toEqual({ s1: "error" });
      vi.advanceTimersByTime(2_500);
      expect(get(dismissedSessionAgentStates)).toEqual({ s1: "error" });
      expect(get(visibleAgentStates)).toEqual({});
    } finally {
      vi.useRealTimers();
    }
  });

  it("keeps active waiting status briefly visible before dismissing it", () => {
    vi.useFakeTimers();
    try {
      sessions.set([
        { id: "s1", name: "codex", parent: { kind: "project", id: "p1" }, cwd: "/repo" },
      ]);
      activeSessionId.set("s1");
      applyHitchEvent({
        type: "agent-state",
        session_id: "s1",
        worktree_id: null,
        agent: "codex",
        state: "waiting",
        detail: null,
      } as any);
      expect(get(visibleAgentStates)).toEqual({ s1: "waiting" });
      vi.advanceTimersByTime(2_499);
      expect(get(visibleAgentStates)).toEqual({ s1: "waiting" });
      vi.advanceTimersByTime(1);
      expect(get(dismissedSessionAgentStates)).toEqual({ s1: "waiting" });
      expect(get(visibleAgentStates)).toEqual({});
    } finally {
      vi.useRealTimers();
    }
  });

  it("keeps needs-approval tab status sticky until state changes or clears", () => {
    sessions.set([
      { id: "s1", name: "codex", parent: { kind: "project", id: "p1" }, cwd: "/repo" },
      { id: "s2", name: "shell", parent: { kind: "project", id: "p1" }, cwd: "/repo" },
    ]);
    activeSessionId.set("s2");
    applyHitchEvent({
      type: "agent-state",
      session_id: "s1",
      worktree_id: null,
      agent: "codex",
      state: "needs-approval",
      detail: null,
    } as any);

    activeSessionId.set("s1");
    expect(get(dismissedSessionAgentStates)).toEqual({});
    expect(get(visibleAgentStates)).toEqual({ s1: "needs-approval" });

    applyHitchEvent({
      type: "agent-state",
      session_id: "s1",
      worktree_id: null,
      agent: "codex",
      state: "waiting",
      detail: null,
    } as any);
    expect(get(visibleAgentStates)).toEqual({ s1: "waiting" });

    applyHitchEvent({
      type: "agent-state",
      session_id: "s1",
      worktree_id: null,
      agent: "codex",
      state: null,
      detail: null,
    } as any);
    expect(get(agentStates)).toEqual({});
    expect(get(dismissedSessionAgentStates)).toEqual({});
  });

  it("drops per-session state when the session closes", () => {
    worktrees.set([
      {
        id: "w1",
        project_id: "p1",
        path: "/repo",
        branch: "main",
        is_main: true,
        is_hitch_managed: false,
      },
    ]);
    sessions.set([
      { id: "s1", name: "claude", parent: { kind: "worktree", id: "w1" }, cwd: "/repo" },
    ]);
    agentStates.set({ s1: "running" });

    applyHitchEvent({ type: "session-closed", session_id: "s1", exit_code: null } as any);
    expect(get(agentStates)).toEqual({});
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
        return { log_path: "/tmp/hitch-daemon.log" };
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

    // The fresher per-worktree lookup then fails; the stale chip must not survive.
    completeJob("j-pw", { type: "error", error: { message: "boom" } });
    await perWorktree;

    expect(get(prByWorktree)["wt-1"]).toBeUndefined();
    expect(get(prInfo)).toBeNull();
  });
});
