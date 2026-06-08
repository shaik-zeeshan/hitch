// Unit tests for scope-aware ingestion + remote Daemon scope status/rollup +
// Retry Now (issue #27, ADR 0014). The desktop attaches to remote Daemons over
// an SSH stdio proxy; the Rust side pushes scope-tagged events (`hitch-scope-event`)
// and per-scope Daemon Status (`hitch-scope-status`), and exposes `set_ssh_hosts`
// / `retry_ssh_host`. These tests drive the frontend seams those listeners call
// (`applyScopeEvent`, `applyScopeStatus`, `retrySshHost`) plus the per-scope
// rollup and the scope-routed `refreshAll`.
//
// Same node-based mocking style as daemon.test.ts / daemonScope.test.ts: the
// Tauri `invoke`/`listen`/`Channel` surface is mocked so the module loads
// without a webview.

import { beforeEach, describe, expect, it, vi } from "vitest";
import { get } from "svelte/store";

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
  agentActRollupByScope,
  agentStates,
  applyScopeEvent,
  applyScopeStatus,
  daemonScopes,
  disposeDaemon,
  projectScopes,
  projects,
  refreshAll,
  retrySshHost,
  scopeForProject,
  scopeForSession,
  selectedProjectId,
  selectedWorktreeId,
  sessions,
  worktrees,
} from "./daemon";
import { sshHosts } from "./sshHosts";
import { LOCAL_SCOPE_ID, type Project, type Session, type Worktree } from "./types";

const HOST = "prod";
const SCOPE = `ssh:${HOST}`;

const project = (id: string, kind: Project["kind"] = "git-backed"): Project => ({
  id,
  name: id,
  root: `/repo/${id}`,
  kind,
});

const worktree = (id: string, projectId: string, isMain = false): Worktree => ({
  id,
  project_id: projectId,
  path: `/repo/${projectId}/${id}`,
  branch: id,
  is_main: isMain,
  is_hitch_managed: !isMain,
});

const session = (id: string, parentId: string): Session => ({
  id,
  name: id,
  parent: { kind: "worktree", id: parentId },
  cwd: `/repo/${parentId}`,
});

// Mock a per-scope snapshot: `local` and a remote scope each get their own
// projects/worktrees/sessions, keyed off the invoke's `scope` arg (undefined =
// local). Mirrors the request shapes refreshAll sends.
function mockScopedSnapshot(scopes: Record<
  string,
  { projects: Project[]; worktrees: Worktree[]; sessions: Session[] }
>): void {
  invokeMock.mockImplementation(
    async (
      command: string,
      payload?: { request?: { type: string; project_id?: string; worktree_id?: string }; scope?: string },
    ) => {
      if (command !== "hitch_request") return undefined;
      const scopeKey = payload?.scope ?? LOCAL_SCOPE_ID;
      const data = scopes[scopeKey] ?? { projects: [], worktrees: [], sessions: [] };
      const request = payload?.request;
      switch (request?.type) {
        case "list-projects":
          return { type: "projects", projects: data.projects };
        case "list-worktrees":
          return {
            type: "worktrees",
            worktrees: data.worktrees.filter((w) => w.project_id === request.project_id),
          };
        case "list-sessions":
          return { type: "sessions", sessions: data.sessions };
        case "git-status":
          return {
            type: "git-status",
            status: {
              worktree_id: request.worktree_id,
              branch: "x",
              dirty: false,
              ahead: 0,
              behind: 0,
              additions: 0,
              deletions: 0,
              files: [],
            },
          };
        case "pr-status":
        case "project-pr-statuses":
          return { type: "noop", pr: null, statuses: [] };
        default:
          throw new Error(`unexpected request ${request?.type}`);
      }
    },
  );
}

beforeEach(() => {
  disposeDaemon();
  invokeMock.mockReset();
  invokeMock.mockResolvedValue(undefined);
  projects.set([]);
  worktrees.set([]);
  sessions.set([]);
  agentStates.set({});
  selectedProjectId.set(null);
  selectedWorktreeId.set(null);
  activeSessionId.set(null);
  projectScopes.set({});
  // One saved SSH Host so its scope row exists in the tree.
  sshHosts.set([{ id: SCOPE, target: HOST }]);
  daemonScopes.set([
    { id: LOCAL_SCOPE_ID, kind: "local", label: "LOCAL", status: "starting" },
    { id: SCOPE, kind: "ssh-host", label: HOST, status: "unreachable" },
  ]);
});

describe("scope-tagged event ingestion homes entities under their host scope", () => {
  it("tags a remote project-updated event under its SSH Host scope", () => {
    applyScopeEvent(SCOPE, { type: "project-updated", project: project("rp1") });
    expect(scopeForProject("rp1")).toBe(SCOPE);
    expect(get(projects).some((p) => p.id === "rp1")).toBe(true);
  });

  it("a local project-updated event stays Local even with a remote host present", () => {
    applyScopeEvent(LOCAL_SCOPE_ID, { type: "project-updated", project: project("lp1") });
    expect(scopeForProject("lp1")).toBe(LOCAL_SCOPE_ID);
  });

  it("tags a remote session-opened under its scope so its IO routes there", () => {
    applyScopeEvent(SCOPE, {
      type: "session-opened",
      session: session("rs1", "rw1"),
    });
    expect(scopeForSession("rs1")).toBe(SCOPE);
    expect(get(sessions).some((s) => s.id === "rs1")).toBe(true);
  });

  it("forgets a remote session's scope tag when it closes", () => {
    applyScopeEvent(SCOPE, { type: "session-opened", session: session("rs1", "rw1") });
    expect(scopeForSession("rs1")).toBe(SCOPE);
    applyScopeEvent(SCOPE, { type: "session-closed", session_id: "rs1" });
    expect(scopeForSession("rs1")).toBe(LOCAL_SCOPE_ID);
  });
});

describe("remote scope refreshAll lands the host's registry under its scope", () => {
  it("merges a remote daemon's projects/worktrees/sessions under its scope without clobbering Local", async () => {
    mockScopedSnapshot({
      [LOCAL_SCOPE_ID]: {
        projects: [project("lp1")],
        worktrees: [worktree("lw1", "lp1", true)],
        sessions: [session("ls1", "lw1")],
      },
      [SCOPE]: {
        projects: [project("rp1")],
        worktrees: [worktree("rw1", "rp1", true)],
        sessions: [session("rs1", "rw1")],
      },
    });

    await refreshAll(); // local
    await refreshAll({ scope: SCOPE }); // remote

    // Both scopes' projects coexist, each tagged to its owner.
    expect(scopeForProject("lp1")).toBe(LOCAL_SCOPE_ID);
    expect(scopeForProject("rp1")).toBe(SCOPE);
    const ids = get(projects).map((p) => p.id).sort();
    expect(ids).toEqual(["lp1", "rp1"]);
    // Remote worktrees + sessions are present and the session is scope-tagged.
    expect(get(worktrees).some((w) => w.id === "rw1")).toBe(true);
    expect(get(sessions).some((s) => s.id === "rs1")).toBe(true);
    expect(scopeForSession("rs1")).toBe(SCOPE);
  });

  it("routes the remote snapshot's requests through the scope arg", async () => {
    mockScopedSnapshot({
      [SCOPE]: {
        projects: [project("rp1")],
        worktrees: [worktree("rw1", "rp1", true)],
        sessions: [],
      },
    });
    await refreshAll({ scope: SCOPE });
    // Every hitch_request for the remote snapshot carried scope=SCOPE.
    const remoteCalls = invokeMock.mock.calls.filter(([cmd]) => cmd === "hitch_request");
    expect(remoteCalls.length).toBeGreaterThan(0);
    for (const [, payload] of remoteCalls) {
      expect((payload as { scope?: string }).scope).toBe(SCOPE);
    }
  });
});

describe("per-scope Daemon Status transitions on the host row", () => {
  it("starting -> running drives the host scope status and snapshots once", async () => {
    mockScopedSnapshot({
      [SCOPE]: { projects: [project("rp1")], worktrees: [], sessions: [] },
    });
    applyScopeStatus(SCOPE, "starting", null);
    expect(get(daemonScopes).find((s) => s.id === SCOPE)?.status).toBe("starting");

    applyScopeStatus(SCOPE, "running", null);
    expect(get(daemonScopes).find((s) => s.id === SCOPE)?.status).toBe("running");
    // The rising edge to running triggers a per-scope snapshot.
    await Promise.resolve();
    await Promise.resolve();
    expect(scopeForProject("rp1")).toBe(SCOPE);

    // A second running (e.g. a heartbeat re-fire) does NOT re-snapshot.
    const callsBefore = invokeMock.mock.calls.length;
    applyScopeStatus(SCOPE, "running", null);
    await Promise.resolve();
    expect(invokeMock.mock.calls.length).toBe(callsBefore);
  });

  it("running -> unreachable marks the host down and re-arms a future snapshot", () => {
    applyScopeStatus(SCOPE, "running", null);
    applyScopeStatus(SCOPE, "unreachable", "ssh dropped");
    expect(get(daemonScopes).find((s) => s.id === SCOPE)?.status).toBe("unreachable");
  });

  it("a protocol mismatch surfaces as failed with the classified reason", () => {
    const reason =
      "Protocol mismatch: this Hitch speaks v3, the remote hitch speaks v2. Update hitch on the host.";
    applyScopeStatus(SCOPE, "failed", reason);
    const scope = get(daemonScopes).find((s) => s.id === SCOPE);
    expect(scope?.status).toBe("failed");
  });
});

describe("per-scope agent act rollup", () => {
  it("rolls up a remote scope's act-state sessions for a collapsed host header", () => {
    // Ingest a remote project + worktree + two sessions, one needs-approval.
    applyScopeEvent(SCOPE, { type: "project-updated", project: project("rp1") });
    worktrees.set([worktree("rw1", "rp1", true)]);
    applyScopeEvent(SCOPE, { type: "session-opened", session: session("rs1", "rw1") });
    applyScopeEvent(SCOPE, { type: "session-opened", session: session("rs2", "rw1") });
    agentStates.set({ rs1: "needs-approval", rs2: "running" });

    const rollup = get(agentActRollupByScope)[SCOPE];
    expect(rollup).toEqual({ state: "needs-approval", count: 1 });
    // Local scope has nothing in an act state.
    expect(get(agentActRollupByScope)[LOCAL_SCOPE_ID]).toBeUndefined();
  });

  it("keeps local and remote rollups separate", () => {
    // Local project/session.
    projects.set([project("lp1"), project("rp1")]);
    projectScopes.set({ lp1: LOCAL_SCOPE_ID, rp1: SCOPE });
    worktrees.set([worktree("lw1", "lp1", true), worktree("rw1", "rp1", true)]);
    sessions.set([session("ls1", "lw1"), session("rs1", "rw1")]);
    agentStates.set({ ls1: "error", rs1: "needs-approval" });

    const byScope = get(agentActRollupByScope);
    expect(byScope[LOCAL_SCOPE_ID]).toEqual({ state: "error", count: 1 });
    expect(byScope[SCOPE]).toEqual({ state: "needs-approval", count: 1 });
  });
});

describe("Retry Now resets backoff via the pool command", () => {
  it("retrySshHost invokes retry_ssh_host with the target", async () => {
    invokeMock.mockResolvedValue(undefined);
    await retrySshHost(HOST);
    expect(invokeMock).toHaveBeenCalledWith("retry_ssh_host", { target: HOST });
  });
});
