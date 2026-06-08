// Unit tests for the daemon-scope state model (issue #25 / ADR 0014). This slice
// is a no-regression cutover to a multi-daemon tree that ships only the Local
// scope: every locally-ingested Project/Worktree/Session is interpreted under
// the Local scope, selection becomes scope-aware, and the tree orders Local
// first. SSH Host scopes (issues #26+) are NOT built yet; these tests pin the
// local-only behavior so adding remote scopes later is a clean extension.
//
// Same node-based mocking style as daemon.test.ts: the Tauri `invoke`/`listen`/
// `Channel` surface is mocked so the module loads without a webview.

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
  agentActRollupByProject,
  agentStateByWorktree,
  agentStates,
  applyDaemonStatus,
  applyHitchEvent,
  daemonScopes,
  daemonScopesOrdered,
  disposeDaemon,
  projectScopes,
  projectsByScope,
  refreshAll,
  scopeForProject,
  selectedProjectId,
  selectedScopeId,
  selectedWorktreeId,
  projects,
  sessions,
  worktrees,
} from "./daemon";
import { sshHosts } from "./sshHosts";
import { LOCAL_SCOPE_ID, type DaemonScope, type Project, type Session, type Worktree } from "./types";

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

// Drive refreshAll with a list-projects / list-worktrees / list-sessions /
// git-status snapshot, the same request shapes daemon.ts sends.
function mockSnapshot(opts: {
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
        case "start-job":
          return { type: "noop", pr: null, statuses: [], job_id: "j" };
        default:
          throw new Error(`unexpected request ${request?.type}`);
      }
    },
  );
}

beforeEach(() => {
  disposeDaemon();
  invokeMock.mockReset();
  projects.set([]);
  worktrees.set([]);
  sessions.set([]);
  agentStates.set({});
  selectedProjectId.set(null);
  selectedWorktreeId.set(null);
  activeSessionId.set(null);
  projectScopes.set({});
  // Clear saved SSH Hosts first: daemon.ts reconciles `daemonScopes` from this
  // store, so an `.set([])` here lands before we pin the Local-only baseline.
  sshHosts.set([]);
  daemonScopes.set([{ id: LOCAL_SCOPE_ID, kind: "local", label: "LOCAL", status: "starting" }]);
});

describe("daemon scopes: Local is the well-known first scope", () => {
  it("seeds a Local scope by default, before any connect", () => {
    const scopes = get(daemonScopes);
    expect(scopes).toHaveLength(1);
    expect(scopes[0]).toMatchObject({ id: LOCAL_SCOPE_ID, kind: "local", label: "LOCAL" });
  });

  it("orders Local first even against an alphabetically-earlier remote scope", () => {
    // Simulate a future SSH Host scope to assert ordering is Local-first, not
    // pure alphabetical (this is the only place this slice fabricates a remote).
    const aardvark: DaemonScope = { id: "ssh:aardvark", kind: "ssh-host", label: "aardvark", status: "running" };
    daemonScopes.update((scopes) => [aardvark, ...scopes]);
    const ordered = get(daemonScopesOrdered).map((s) => s.id);
    expect(ordered[0]).toBe(LOCAL_SCOPE_ID);
    expect(ordered).toContain("ssh:aardvark");
  });

  it("mirrors the local Daemon Status onto the Local scope", () => {
    applyDaemonStatus("running", null);
    expect(get(daemonScopes).find((s) => s.id === LOCAL_SCOPE_ID)?.status).toBe("running");
    applyDaemonStatus("failed", "boom");
    expect(get(daemonScopes).find((s) => s.id === LOCAL_SCOPE_ID)?.status).toBe("failed");
  });
});

describe("entity ingestion is interpreted under the Local scope", () => {
  it("tags every project from a snapshot as Local", async () => {
    mockSnapshot({
      projects: [project("p1"), project("p2", "plain")],
      worktrees: [worktree("w1", "p1", true)],
      sessions: [],
    });
    await refreshAll();

    expect(scopeForProject("p1")).toBe(LOCAL_SCOPE_ID);
    expect(scopeForProject("p2")).toBe(LOCAL_SCOPE_ID);
    expect(get(projectScopes)).toEqual({ p1: LOCAL_SCOPE_ID, p2: LOCAL_SCOPE_ID });
  });

  it("groups projects by scope with all locals under Local", async () => {
    mockSnapshot({
      projects: [project("p1"), project("p2")],
      worktrees: [worktree("w1", "p1", true)],
      sessions: [],
    });
    await refreshAll();

    const grouped = get(projectsByScope);
    expect(grouped[LOCAL_SCOPE_ID]?.map((p) => p.id)).toEqual(["p1", "p2"]);
    // No other scope buckets exist when only Local has ingested projects.
    expect(Object.keys(grouped)).toEqual([LOCAL_SCOPE_ID]);
  });

  it("tags a project pushed via a project-updated event as Local", () => {
    applyHitchEvent({ type: "project-updated", project: project("p9") });
    expect(scopeForProject("p9")).toBe(LOCAL_SCOPE_ID);
  });

  it("untags a project removed via a project-removed event", async () => {
    mockSnapshot({ projects: [project("p1")], worktrees: [], sessions: [] });
    await refreshAll();
    expect(scopeForProject("p1")).toBe(LOCAL_SCOPE_ID);

    applyHitchEvent({ type: "project-removed", project_id: "p1" });
    expect(get(projectScopes)).toEqual({});
  });

  it("falls back to Local for an untagged project id", () => {
    expect(scopeForProject("never-ingested")).toBe(LOCAL_SCOPE_ID);
    expect(scopeForProject(null)).toBe(LOCAL_SCOPE_ID);
  });
});

describe("selection is scope-aware", () => {
  it("resolves the selected scope from the selected project's owning scope", async () => {
    mockSnapshot({
      projects: [project("p1")],
      worktrees: [worktree("w1", "p1", true)],
      sessions: [],
    });
    await refreshAll();

    expect(get(selectedScopeId)).toBe(LOCAL_SCOPE_ID); // no selection → Local default
    selectedProjectId.set("p1");
    expect(get(selectedScopeId)).toBe(LOCAL_SCOPE_ID);
  });

  it("defaults the selected scope to Local with nothing selected", () => {
    selectedProjectId.set(null);
    expect(get(selectedScopeId)).toBe(LOCAL_SCOPE_ID);
  });
});

describe("rollups are unchanged under Local scoping", () => {
  it("still rolls up per-project act state and per-worktree agent state", () => {
    projects.set([project("p1")]);
    projectScopes.set({ p1: LOCAL_SCOPE_ID });
    worktrees.set([worktree("w1", "p1", true)]);
    sessions.set([session("s1", "w1")]);
    agentStates.set({ s1: "needs-approval" });

    expect(get(agentActRollupByProject).p1).toEqual({ state: "needs-approval", count: 1 });
    expect(get(agentStateByWorktree).w1).toBe("needs-approval");
  });
});

describe("dispose resets scope state to Local plus saved SSH Hosts", () => {
  it("clears project tags but keeps Local and persisted SSH Host scopes", async () => {
    mockSnapshot({ projects: [project("p1")], worktrees: [], sessions: [] });
    await refreshAll();
    sshHosts.set([{ id: "ssh:prod", target: "prod" }]);

    disposeDaemon();

    expect(get(projectScopes)).toEqual({});
    // Local + the saved host survive (hosts are GUI-local config, not daemon
    // state); only project tags and Local's running status reset.
    const ids = get(daemonScopes).map((s) => s.id);
    expect(ids).toContain(LOCAL_SCOPE_ID);
    expect(ids).toContain("ssh:prod");
  });
});

describe("SSH Host scopes seed the tree (issue #26)", () => {
  it("seeds a saved host as a top-level ssh-host scope, ordered after Local", () => {
    sshHosts.set([{ id: "ssh:user@example.com", target: "user@example.com" }]);
    const ordered = get(daemonScopesOrdered);
    expect(ordered.map((s) => s.id)).toEqual([LOCAL_SCOPE_ID, "ssh:user@example.com"]);
    const host = ordered[1];
    expect(host).toMatchObject({ kind: "ssh-host", label: "user@example.com", status: "unreachable" });
  });

  it("orders multiple SSH Hosts alphabetically by target, Local always first", () => {
    sshHosts.set([
      { id: "ssh:zulu", target: "zulu" },
      { id: "ssh:alpha", target: "alpha" },
    ]);
    expect(get(daemonScopesOrdered).map((s) => s.label)).toEqual(["LOCAL", "alpha", "zulu"]);
  });

  it("preserves a host's live status across a re-seed (issue #27 seam)", () => {
    sshHosts.set([{ id: "ssh:prod", target: "prod" }]);
    // Simulate issue #27 setting a real Daemon Status on the host scope.
    daemonScopes.update((scopes) =>
      scopes.map((s) => (s.id === "ssh:prod" ? { ...s, status: "running" } : s)),
    );
    // A re-seed (e.g. another host added) must not clobber the live status.
    sshHosts.update((hosts) => [...hosts, { id: "ssh:edge", target: "edge" }]);
    const prod = get(daemonScopes).find((s) => s.id === "ssh:prod");
    expect(prod?.status).toBe("running");
  });

  it("drops a removed host's scope", () => {
    sshHosts.set([
      { id: "ssh:a", target: "a" },
      { id: "ssh:b", target: "b" },
    ]);
    sshHosts.update((hosts) => hosts.filter((h) => h.id !== "ssh:a"));
    const ids = get(daemonScopes).map((s) => s.id);
    expect(ids).toContain("ssh:b");
    expect(ids).not.toContain("ssh:a");
  });

  it("mirroring the local Daemon Status does not disturb SSH Host scopes", () => {
    sshHosts.set([{ id: "ssh:prod", target: "prod" }]);
    applyDaemonStatus("running", null);
    const prod = get(daemonScopes).find((s) => s.id === "ssh:prod");
    // Local updated; the host's placeholder status is untouched.
    expect(get(daemonScopes).find((s) => s.id === LOCAL_SCOPE_ID)?.status).toBe("running");
    expect(prod?.status).toBe("unreachable");
  });
});
