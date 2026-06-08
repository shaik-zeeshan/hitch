// Unit tests for the multi-daemon navigation + stale-remote polish (issue #32,
// ADR 0014). Covers the daemon-layer half of the slice:
//   - scope liveness store (`liveScopes`) flips with Daemon Status, and a stale
//     scope's keystrokes are no-op'd (the PTY is unreachable),
//   - reconnect REPLACES (prunes) a remote scope's entities that vanished on the
//     host while disconnected, instead of merging stale orphans back in,
//   - a remote scope dropping out of `running` fails its in-flight Jobs with a
//     clear host-named reason (local Jobs untouched),
//   - removing an SSH Host forgets only the GUI-local entities/sessions/jobs and
//     leaves the remote daemon assumption intact (no kill request is sent),
//   - top-level scope order is stable (Local first, hosts alpha by target) across
//     status changes.
//
// The palette-metadata + collapsed-rollup display rules are pure functions tested
// in scopeCopy.test.ts; here we cover the store-driven behaviors. Same node-based
// mocking style as remoteSessions.test.ts: the Tauri surface is mocked headless.

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
vi.mock("@tauri-apps/plugin-notification", () => ({
  isPermissionGranted: vi.fn(async () => true),
  requestPermission: vi.fn(async () => "granted"),
  sendNotification: vi.fn(),
}));

import {
  activeSessionId,
  agentStates,
  applyScopeEvent,
  applyScopeStatus,
  daemonScopes,
  daemonScopesOrdered,
  disposeDaemon,
  forgetRemoteScope,
  jobs,
  liveScopes,
  projectScopes,
  projects,
  refreshAll,
  runJob,
  scopeForSession,
  scopeIsLive,
  selectedProjectId,
  selectedWorktreeId,
  sendInput,
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
const worktree = (id: string, projectId: string, branch = id): Worktree => ({
  id,
  project_id: projectId,
  path: `/repo/${projectId}/${id}`,
  branch,
  is_main: false,
  is_hitch_managed: true,
});
const session = (id: string, parentId: string): Session => ({
  id,
  name: id,
  parent: { kind: "worktree", id: parentId },
  cwd: `/repo/${parentId}`,
});

// Drive a per-scope refreshAll snapshot. The remote daemon's `list-*` responses
// are served from `opts`; `git-status` returns a clean status for any worktree.
function mockScopeSnapshot(opts: {
  projects: Project[];
  worktrees: Worktree[];
  sessions: Session[];
}): void {
  invokeMock.mockImplementation(
    async (command: string, payload?: { request?: { type: string; project_id?: string } }) => {
      if (command !== "hitch_request") return undefined;
      const request = payload?.request;
      switch (request?.type) {
        case "list-projects":
          return { type: "projects", projects: opts.projects };
        case "list-worktrees":
          return {
            type: "worktrees",
            worktrees: opts.worktrees.filter((w) => w.project_id === request.project_id),
          };
        case "list-sessions":
          return { type: "sessions", sessions: opts.sessions };
        case "git-status":
          return {
            type: "git-status",
            status: {
              worktree_id: (request as unknown as { worktree_id: string }).worktree_id,
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
          return undefined;
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
  projectScopes.set({});
  jobs.set({});
  selectedProjectId.set(null);
  selectedWorktreeId.set(null);
  activeSessionId.set(null);
  sshHosts.set([{ id: SCOPE, target: HOST }]);
  daemonScopes.set([
    { id: LOCAL_SCOPE_ID, kind: "local", label: "LOCAL", status: "running" },
    { id: SCOPE, kind: "ssh-host", label: HOST, status: "running" },
  ]);
});

// Seed a remote project + worktree + session owned by the SSH Host scope as if a
// prior snapshot had ingested them (the "last known tree" before a drop).
function seedRemoteTree(): void {
  projects.set([project("rp1")]);
  worktrees.set([worktree("rw1", "rp1", "feature")]);
  projectScopes.set({ rp1: SCOPE });
  applyScopeEvent(SCOPE, { type: "session-opened", session: session("rs1", "rw1") });
}

describe("scope liveness store flips with Daemon Status", () => {
  it("marks the host live while running and stale when it leaves running", () => {
    expect(get(liveScopes).has(SCOPE)).toBe(true);
    expect(scopeIsLive(SCOPE)).toBe(true);

    applyScopeStatus(SCOPE, "unreachable", "vpn down");
    expect(get(liveScopes).has(SCOPE)).toBe(false);
    expect(scopeIsLive(SCOPE)).toBe(false);
    // Local is unaffected — it stays live.
    expect(get(liveScopes).has(LOCAL_SCOPE_ID)).toBe(true);
  });

  it("no-ops a stale remote session's keystrokes (PTY unreachable)", () => {
    seedRemoteTree();
    applyScopeStatus(SCOPE, "unreachable", "vpn down");
    invokeMock.mockClear();

    sendInput("rs1", "ls\n");
    // No input frame is sent to a dead PTY.
    expect(invokeMock.mock.calls.some(([c]) => c === "send_session_input")).toBe(false);
  });

  it("still sends a live remote session's keystrokes", () => {
    seedRemoteTree();
    invokeMock.mockClear();
    sendInput("rs1", "ls\n");
    expect(invokeMock).toHaveBeenCalledWith("send_session_input", {
      sessionId: "rs1",
      data: "ls\n",
      scope: SCOPE,
    });
  });

  it("keeps the host's last tree in the stores while stale (greyed, not pruned)", () => {
    seedRemoteTree();
    applyScopeStatus(SCOPE, "failed", "proxy exited");
    // Entities stay (the tree greys as stale UI; it is NOT removed on disconnect).
    expect(get(projects).map((p) => p.id)).toContain("rp1");
    expect(get(worktrees).map((w) => w.id)).toContain("rw1");
    expect(get(sessions).map((s) => s.id)).toContain("rs1");
  });
});

describe("reconnect REPLACES the stale tree (prunes vanished entities)", () => {
  it("drops a remote project/worktree/session that disappeared on the host", async () => {
    // Last known tree: two projects, each with a worktree + session.
    projects.set([project("rp1"), project("rp2")]);
    worktrees.set([worktree("rw1", "rp1", "feature"), worktree("rw2", "rp2", "bugfix")]);
    projectScopes.set({ rp1: SCOPE, rp2: SCOPE });
    applyScopeEvent(SCOPE, { type: "session-opened", session: session("rs1", "rw1") });
    applyScopeEvent(SCOPE, { type: "session-opened", session: session("rs2", "rw2") });
    agentStates.set({ rs1: "needs-approval", rs2: "running" });

    // Host drops, then reconnects with rp2/rw2/rs2 GONE (removed out of band).
    applyScopeStatus(SCOPE, "unreachable", "drop");
    mockScopeSnapshot({
      projects: [project("rp1")],
      worktrees: [worktree("rw1", "rp1", "feature")],
      sessions: [session("rs1", "rw1")],
    });
    applyScopeStatus(SCOPE, "running", null);
    // applyScopeStatus fires refreshAll fire-and-forget; flush microtasks.
    await new Promise((r) => setTimeout(r, 0));

    // rp1/rw1/rs1 survive the replay; rp2/rw2/rs2 are pruned, not merged back.
    expect(get(projects).map((p) => p.id)).toEqual(["rp1"]);
    expect(get(worktrees).map((w) => w.id)).toEqual(["rw1"]);
    expect(get(sessions).map((s) => s.id)).toEqual(["rs1"]);
    // The pruned session's agent state is swept (so it can't keep paging).
    expect(get(agentStates)["rs2"]).toBeUndefined();
    expect(get(agentStates)["rs1"]).toBe("needs-approval");
    // The pruned project loses its scope tag.
    expect(get(projectScopes)).toEqual({ rp1: SCOPE });
  });

  it("does not disturb Local entities when a remote scope re-snapshots", async () => {
    projects.set([project("lp1"), project("rp1")]);
    worktrees.set([worktree("lw1", "lp1", "main"), worktree("rw1", "rp1", "feature")]);
    projectScopes.set({ rp1: SCOPE }); // lp1 is Local

    applyScopeStatus(SCOPE, "unreachable", "drop");
    mockScopeSnapshot({ projects: [], worktrees: [], sessions: [] });
    applyScopeStatus(SCOPE, "running", null);
    await new Promise((r) => setTimeout(r, 0));

    // The remote scope emptied; Local is untouched.
    expect(get(projects).map((p) => p.id)).toEqual(["lp1"]);
    expect(get(worktrees).map((w) => w.id)).toEqual(["lw1"]);
  });
});

describe("a remote scope drop fails its in-flight Jobs", () => {
  it("rejects the scoped Job with a host-named reason and clears it", async () => {
    invokeMock.mockImplementation(async (_c, { request }: { request: { type: string } }) =>
      request.type === "start-job" ? { type: "job-started", job_id: "j1" } : undefined,
    );
    let rejected: string | null = null;
    const promise = runJob({ type: "push", worktree_id: "rw1" }, "push", SCOPE).catch(
      (e: Error) => (rejected = e.message),
    );
    await Promise.resolve();
    await Promise.resolve();
    expect(get(jobs)[`${SCOPE}:j1`]).toBeTruthy();

    applyScopeStatus(SCOPE, "unreachable", "vpn down");
    await promise;

    expect(rejected).toContain(HOST);
    expect(rejected).toContain("lost");
    expect(get(jobs)[`${SCOPE}:j1`]).toBeUndefined();
  });

  it("leaves a Local Job untouched when a remote scope drops", async () => {
    invokeMock.mockImplementation(async (_c, { request }: { request: { type: string } }) =>
      request.type === "start-job" ? { type: "job-started", job_id: "jl" } : undefined,
    );
    let settled = false;
    void runJob({ type: "push", worktree_id: "lw1" }, "push", LOCAL_SCOPE_ID)
      .then(() => (settled = true))
      .catch(() => (settled = true));
    await Promise.resolve();
    await Promise.resolve();
    expect(get(jobs)["jl"]).toBeTruthy();

    applyScopeStatus(SCOPE, "failed", "proxy exited");
    await Promise.resolve();
    // The Local job is still live (not failed by a remote scope's drop).
    expect(get(jobs)["jl"]).toBeTruthy();
    expect(settled).toBe(false);
  });
});

describe("removing an SSH Host forgets only GUI-local state", () => {
  it("prunes the host's entities/sessions and never sends a remote kill", () => {
    seedRemoteTree();
    selectedProjectId.set("rp1");
    selectedWorktreeId.set("rw1");
    activeSessionId.set("rs1");
    invokeMock.mockClear();

    forgetRemoteScope(SCOPE);

    // Entities + scope tags pruned from every store.
    expect(get(projects).map((p) => p.id)).not.toContain("rp1");
    expect(get(worktrees).map((w) => w.id)).not.toContain("rw1");
    expect(get(sessions).map((s) => s.id)).not.toContain("rs1");
    expect(get(projectScopes)["rp1"]).toBeUndefined();
    expect(scopeForSession("rs1")).toBe(LOCAL_SCOPE_ID); // tag dropped → Local default
    // Selection that pointed into the removed scope falls back sanely.
    expect(get(selectedProjectId)).toBeNull();
    expect(get(selectedWorktreeId)).toBeNull();
    expect(get(activeSessionId)).toBeNull();

    // The GUI never shuts the remote daemon down or kills its sessions — only the
    // local output channel is unregistered (a GUI teardown), and the pool
    // disconnect is driven by the sshHosts subscription, not a request here.
    expect(
      invokeMock.mock.calls.some(
        ([c, p]) =>
          c === "hitch_request" &&
          ["close-session", "remove-project", "remove-worktree"].includes(
            (p as { request?: { type?: string } })?.request?.type ?? "",
          ),
      ),
    ).toBe(false);
  });

  it("is a no-op for the Local scope", () => {
    projects.set([project("lp1")]);
    worktrees.set([worktree("lw1", "lp1")]);
    forgetRemoteScope(LOCAL_SCOPE_ID);
    expect(get(projects).map((p) => p.id)).toEqual(["lp1"]);
    expect(get(worktrees).map((w) => w.id)).toEqual(["lw1"]);
  });
});

describe("top-level scope order is stable across status changes", () => {
  it("keeps Local first and hosts alpha by target through status flips", () => {
    sshHosts.set([
      { id: "ssh:zulu", target: "zulu" },
      { id: "ssh:alpha", target: "alpha" },
    ]);
    const order = () => get(daemonScopesOrdered).map((s) => s.label);
    expect(order()).toEqual(["LOCAL", "alpha", "zulu"]);

    // A host going unreachable must not reorder the tree.
    applyScopeStatus("ssh:zulu", "unreachable", "drop");
    expect(order()).toEqual(["LOCAL", "alpha", "zulu"]);
    applyScopeStatus("ssh:alpha", "failed", "drop");
    expect(order()).toEqual(["LOCAL", "alpha", "zulu"]);
    applyScopeStatus("ssh:zulu", "running", null);
    expect(order()).toEqual(["LOCAL", "alpha", "zulu"]);
  });
});
