// Unit tests for scope-routed git / Worktree / Job operations (issue #30, ADR
// 0014). Git status/diff/log/commit/stage/push/pull/fetch/PR, create/remove
// worktree, remove project, and list-branches all route to the OWNING daemon
// scope; Job ids are interpreted within their daemon scope so two daemons can mint
// the same JobId without colliding. Local entities keep passing NO scope (the
// back-compat path) — every remote assertion has a local counterpart.
//
// Same node-based mocking style as remoteSessions.test.ts / sshPool.test.ts: the
// Tauri `invoke`/`listen`/`Channel` surface is mocked so the module loads headless.

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
  applyJobProgress,
  applyScopeEvent,
  cancelJob,
  commit,
  completeJob,
  createWorktree,
  dirtyWorktrees,
  discardFile,
  disposeDaemon,
  fetchRemote,
  jobs,
  listBranches,
  loadCommitLog,
  loadPrStatus,
  projectScopes,
  projects,
  push,
  pull,
  removeProject,
  removeWorktree,
  runJob,
  scopeAttributionForProject,
  scopeAttributionForWorktree,
  selectedProjectId,
  selectedWorktreeId,
  setFileStaged,
  viewDiff,
  worktrees,
} from "./daemon";
import { railView } from "./settings";
import { sshHosts } from "./sshHosts";
import { LOCAL_SCOPE_ID, type Project, type Worktree } from "./types";

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

// The last `request`/`scope` an `hitch_request` invoke carried. The Local
// back-compat path leaves `scope` undefined.
function lastHitchRequest(): { request: { type: string; [k: string]: unknown }; scope: string | undefined } | null {
  for (let i = invokeMock.mock.calls.length - 1; i >= 0; i--) {
    const [command, payload] = invokeMock.mock.calls[i] as [
      string,
      { request?: { type: string }; scope?: string },
    ];
    if (command === "hitch_request") {
      return { request: payload.request as { type: string }, scope: payload.scope };
    }
  }
  return null;
}

// The `hitch_request` whose wrapped request (or StartJob inner request) matches
// `type`, for asserting a specific op's scope when several requests fire.
function hitchRequestOfType(
  type: string,
): { request: { type: string; [k: string]: unknown }; scope: string | undefined } | null {
  for (let i = invokeMock.mock.calls.length - 1; i >= 0; i--) {
    const [command, payload] = invokeMock.mock.calls[i] as [
      string,
      { request?: { type: string; request?: { type: string } }; scope?: string },
    ];
    if (command !== "hitch_request" || !payload.request) continue;
    const req = payload.request;
    const inner = req.type === "start-job" ? req.request : req;
    if (inner?.type === type) {
      return { request: inner as { type: string }, scope: payload.scope };
    }
  }
  return null;
}

beforeEach(() => {
  disposeDaemon();
  invokeMock.mockReset();
  invokeMock.mockResolvedValue(undefined);
  projects.set([]);
  worktrees.set([]);
  jobs.set({});
  projectScopes.set({});
  selectedProjectId.set(null);
  selectedWorktreeId.set(null);
  railView.set("changes");
  sshHosts.set([{ id: SCOPE, target: HOST }]);
});

// A remote git-backed project + worktree owned by the SSH Host scope.
function seedRemote(): void {
  projects.set([project("rp1")]);
  worktrees.set([worktree("rw1", "rp1", "feature")]);
  projectScopes.set({ rp1: SCOPE });
}

// A local git-backed project + worktree (no scope tag = Local).
function seedLocal(): void {
  projects.update((p) => [...p, project("lp1")]);
  worktrees.update((w) => [...w, worktree("lw1", "lp1", "main")]);
}

describe("scope-keyed Job state: same JobId from two daemons", () => {
  it("tracks two same-id jobs independently and resolves the right promise", async () => {
    seedRemote();
    seedLocal();

    // Both daemons mint the SAME JobId "j-dup" for a push (ids are unique only per
    // daemon, ADR 0014). The local job starts via runJob (no scope); the remote via
    // runJob with its host scope. Each StartJob returns "j-dup".
    invokeMock.mockImplementation(async (_command, { request }: { request: { type: string } }) =>
      request.type === "start-job" ? { type: "job-started", job_id: "j-dup" } : undefined,
    );

    let localResolved: string | null = null;
    let remoteResolved: string | null = null;
    const localPromise = runJob<{ type: string; tag: string }>(
      { type: "push", worktree_id: "lw1" },
      "push",
      LOCAL_SCOPE_ID,
    ).then((r) => (localResolved = r.tag));
    const remotePromise = runJob<{ type: string; tag: string }>(
      { type: "push", worktree_id: "rw1" },
      "push",
      SCOPE,
    ).then((r) => (remoteResolved = r.tag));
    await Promise.resolve();
    await Promise.resolve();

    // Two distinct live entries despite the colliding JobId.
    const live = get(jobs);
    expect(live["j-dup"]).toMatchObject({ worktreeId: "lw1", scopeId: LOCAL_SCOPE_ID });
    expect(live[`${SCOPE}:j-dup`]).toMatchObject({ worktreeId: "rw1", scopeId: SCOPE });

    // A scope-tagged progress event updates ONLY the matching entry.
    applyJobProgress("j-dup", "running", "remote pushing", "push", SCOPE);
    expect(get(jobs)[`${SCOPE}:j-dup`].message).toBe("remote pushing");
    expect(get(jobs)["j-dup"].message).toBeNull();

    // Completing the remote job resolves the remote promise only.
    completeJob("j-dup", { type: "ack", tag: "remote" }, SCOPE);
    await remotePromise;
    expect(remoteResolved).toBe("remote");
    expect(localResolved).toBeNull();
    expect(get(jobs)[`${SCOPE}:j-dup`]).toBeUndefined();
    expect(get(jobs)["j-dup"]).toBeTruthy();

    // Completing the local job (same id, Local scope) resolves the local promise.
    completeJob("j-dup", { type: "ack", tag: "local" }, LOCAL_SCOPE_ID);
    await localPromise;
    expect(localResolved).toBe("local");
    expect(get(jobs)["j-dup"]).toBeUndefined();
  });
});

describe("remote git job progress/completion are scope-tagged", () => {
  it("a remote push's scope-tagged progress + completion update the scoped entry", async () => {
    seedRemote();
    invokeMock.mockImplementation(async (_command, { request }: { request: { type: string } }) =>
      request.type === "start-job" ? { type: "job-started", job_id: "j-push" } : undefined,
    );

    const promise = push("rw1");
    await Promise.resolve();
    await Promise.resolve();

    // The StartJob routed to the host scope.
    const start = hitchRequestOfType("push");
    expect(start?.scope).toBe(SCOPE);
    expect(get(jobs)[`${SCOPE}:j-push`]).toMatchObject({ worktreeId: "rw1", scopeId: SCOPE });

    applyJobProgress("j-push", "running", "Pushing…", "push", SCOPE);
    expect(get(jobs)[`${SCOPE}:j-push`].status).toBe("running");

    completeJob("j-push", { type: "ack" }, SCOPE);
    await promise;
    expect(get(jobs)[`${SCOPE}:j-push`]).toBeUndefined();
  });

  it("a remote fetch routes its StartJob to the host scope", async () => {
    seedRemote();
    invokeMock.mockImplementation(async (_command, { request }: { request: { type: string } }) =>
      request.type === "start-job" ? { type: "job-started", job_id: "j-fetch" } : undefined,
    );
    const promise = fetchRemote("rw1");
    await Promise.resolve();
    expect(hitchRequestOfType("fetch")?.scope).toBe(SCOPE);
    completeJob("j-fetch", { type: "ack" }, SCOPE);
    await promise;
  });

  it("a local pull routes with NO scope (back-compat)", async () => {
    seedLocal();
    invokeMock.mockImplementation(async (_command, { request }: { request: { type: string } }) =>
      request.type === "start-job" ? { type: "job-started", job_id: "j-pull" } : undefined,
    );
    const promise = pull("lw1");
    await Promise.resolve();
    expect(hitchRequestOfType("pull")?.scope).toBeUndefined();
    completeJob("j-pull", { type: "ack" }, LOCAL_SCOPE_ID);
    await promise;
    expect(get(jobs)["j-pull"]).toBeUndefined();
  });
});

describe("remote git reads route to the host scope", () => {
  it("git-diff for a remote worktree carries the host scope", async () => {
    seedRemote();
    selectedProjectId.set("rp1");
    selectedWorktreeId.set("rw1");
    invokeMock.mockResolvedValue({ type: "diff", diff: { diff: "@@ -1 +1 @@" } });

    await viewDiff("src/x.ts");
    const diff = hitchRequestOfType("git-diff");
    expect(diff?.scope).toBe(SCOPE);
    expect(diff?.request.worktree_id).toBe("rw1");
  });

  it("git-log for a remote worktree carries the host scope", async () => {
    seedRemote();
    selectedProjectId.set("rp1");
    selectedWorktreeId.set("rw1");
    invokeMock.mockResolvedValue({ type: "commit-log", commits: [], has_more: false });

    await loadCommitLog();
    const log = hitchRequestOfType("git-log");
    expect(log?.scope).toBe(SCOPE);
    expect(log?.request.worktree_id).toBe("rw1");
  });

  it("commit for a remote worktree carries the host scope", async () => {
    seedRemote();
    selectedProjectId.set("rp1");
    selectedWorktreeId.set("rw1");
    // commit() does a follow-up git-status load; give git-status a real status.
    invokeMock.mockImplementation(async (_command, { request }: { request: { type: string } }) =>
      request.type === "git-status"
        ? { type: "status", status: { worktree_id: "rw1", branch: "feature", dirty: false, ahead: 0, behind: 0, additions: 0, deletions: 0, files: [] } }
        : { type: "ack" },
    );

    await commit("subject", null, "rw1");
    expect(hitchRequestOfType("commit")?.scope).toBe(SCOPE);
  });

  it("stage-files for a remote worktree carries the host scope", async () => {
    seedRemote();
    selectedProjectId.set("rp1");
    selectedWorktreeId.set("rw1");
    invokeMock.mockResolvedValue({ type: "ack" });

    await setFileStaged("src/x.ts", true);
    expect(hitchRequestOfType("stage-files")?.scope).toBe(SCOPE);
  });

  it("git-diff for a local worktree carries NO scope (back-compat)", async () => {
    seedLocal();
    selectedProjectId.set("lp1");
    selectedWorktreeId.set("lw1");
    invokeMock.mockResolvedValue({ type: "diff", diff: { diff: "@@ -1 +1 @@" } });

    await viewDiff("src/y.ts");
    const diff = hitchRequestOfType("git-diff");
    expect(diff?.scope).toBeUndefined();
    expect(diff?.request.worktree_id).toBe("lw1");
  });

  it("discard-files for a remote worktree carries the host scope", async () => {
    seedRemote();
    selectedProjectId.set("rp1");
    selectedWorktreeId.set("rw1");
    invokeMock.mockResolvedValue({ type: "ack" });

    await discardFile("src/x.ts");
    expect(hitchRequestOfType("discard-files")?.scope).toBe(SCOPE);
  });

  it("remote PR status runs its lookup on the host daemon", async () => {
    seedRemote();
    invokeMock.mockImplementation(async (_command, { request }: { request: { type: string } }) =>
      request.type === "start-job" ? { type: "job-started", job_id: "j-pr" } : undefined,
    );
    const promise = loadPrStatus("rw1");
    await Promise.resolve();
    expect(hitchRequestOfType("pr-status")?.scope).toBe(SCOPE);
    completeJob("j-pr", { type: "pr", pr: null }, SCOPE);
    await promise;
  });
});

describe("remote worktree create routes list-branches + create-worktree to the host", () => {
  it("list-branches for a remote project carries the host scope", async () => {
    seedRemote();
    invokeMock.mockResolvedValue({ type: "branches", branches: [] });
    await listBranches("rp1");
    const branches = hitchRequestOfType("list-branches");
    expect(branches?.scope).toBe(SCOPE);
    expect(branches?.request.project_id).toBe("rp1");
  });

  it("create-worktree for a remote project routes its Job to the host and lands the worktree", async () => {
    seedRemote();
    invokeMock.mockImplementation(async (_command, { request }: { request: { type: string } }) =>
      request.type === "start-job" ? { type: "job-started", job_id: "j-cw" } : undefined,
    );

    const promise = createWorktree("rp1", "feat/new", "main", "new-branch");
    await Promise.resolve();
    expect(hitchRequestOfType("create-worktree")?.scope).toBe(SCOPE);

    completeJob(
      "j-cw",
      {
        type: "worktrees",
        worktrees: [worktree("rw-new", "rp1", "feat/new")],
      },
      SCOPE,
    );
    const created = await promise;
    expect(created?.id).toBe("rw-new");
    // The created worktree's owning project is the remote one, so it resolves to
    // the host scope (lands under the remote project).
    expect(scopeAttributionForWorktree("rw-new").scopeId).toBe(SCOPE);
  });

  it("list-branches for a local project carries NO scope (back-compat)", async () => {
    seedLocal();
    invokeMock.mockResolvedValue({ type: "branches", branches: [] });
    await listBranches("lp1");
    expect(hitchRequestOfType("list-branches")?.scope).toBeUndefined();
  });
});

describe("remote worktree remove + project remove route to the host", () => {
  it("remove-worktree for a remote worktree carries the host scope", async () => {
    seedRemote();
    invokeMock.mockResolvedValue({ type: "ack" });
    await removeWorktree("rw1", false, true);
    const req = lastHitchRequest();
    expect(req?.scope).toBe(SCOPE);
    expect(req?.request.type).toBe("remove-worktree");
  });

  it("remove-worktree for a local worktree carries NO scope (back-compat)", async () => {
    seedLocal();
    invokeMock.mockResolvedValue({ type: "ack" });
    await removeWorktree("lw1", false, true);
    const req = lastHitchRequest();
    expect(req?.scope).toBeUndefined();
    expect(req?.request.type).toBe("remove-worktree");
  });

  it("remove-project for a remote project routes the request + scoped refresh to the host", async () => {
    seedRemote();
    invokeMock.mockImplementation(async (_command, { request }: { request: { type: string } }) => {
      switch (request.type) {
        case "remove-project":
          return { type: "ack" };
        case "list-projects":
          return { type: "projects", projects: [] };
        case "list-sessions":
          return { type: "sessions", sessions: [] };
        default:
          return undefined;
      }
    });

    await removeProject("rp1", true);
    expect(hitchRequestOfType("remove-project")?.scope).toBe(SCOPE);
    // The follow-up refreshAll routed to the same scope (a remote merge, not a
    // Local wholesale replace).
    const listProjects = hitchRequestOfType("list-projects");
    expect(listProjects?.scope).toBe(SCOPE);
  });
});

describe("cancelJob routes the cancel to the Job's owning daemon", () => {
  it("a remote Job's cancel carries the host scope", async () => {
    invokeMock.mockResolvedValue({ type: "ack" });
    await cancelJob("j-remote", SCOPE);
    const req = lastHitchRequest();
    expect(req?.scope).toBe(SCOPE);
    expect(req?.request).toMatchObject({ type: "cancel-job", job_id: "j-remote" });
  });

  it("a local Job's cancel carries NO scope (back-compat)", async () => {
    invokeMock.mockResolvedValue({ type: "ack" });
    await cancelJob("j-local");
    const req = lastHitchRequest();
    expect(req?.scope).toBeUndefined();
    expect(req?.request).toMatchObject({ type: "cancel-job", job_id: "j-local" });
  });
});

describe("worktree-dirty event scope guard (ADR 0014 id-collision)", () => {
  it("ignores a dirty event from a foreign scope for a same-id worktree", () => {
    // Two daemons each own a worktree with the SAME id "w1" (unique only per
    // daemon). The LOCAL one is selected/known; a dirty event tagged to the REMOTE
    // scope must not flip the LOCAL worktree's dirty flag.
    projects.set([project("lp1"), project("rp1")]);
    worktrees.set([worktree("w1", "lp1", "main")]); // only the LOCAL w1 is ingested
    projectScopes.set({ rp1: SCOPE });
    dirtyWorktrees.set({ w1: false });
    invokeMock.mockResolvedValue({ type: "status", status: { worktree_id: "w1", branch: "main", dirty: true, ahead: 0, behind: 0, additions: 0, deletions: 0, files: [] } });

    // A remote-scope dirty event for "w1" — but the known w1 is Local-owned.
    applyScopeEvent(SCOPE, { type: "worktree-dirty", worktree_id: "w1", dirty: true });
    expect(get(dirtyWorktrees)["w1"]).toBe(false);
  });

  it("applies a dirty event from the owning scope", () => {
    seedRemote();
    dirtyWorktrees.set({ rw1: false });
    invokeMock.mockResolvedValue({ type: "status", status: { worktree_id: "rw1", branch: "feature", dirty: true, ahead: 0, behind: 0, additions: 0, deletions: 0, files: [] } });

    applyScopeEvent(SCOPE, { type: "worktree-dirty", worktree_id: "rw1", dirty: true });
    expect(get(dirtyWorktrees)["rw1"]).toBe(true);
  });
});

describe("scope attribution helpers resolve owning scope for destructive copy", () => {
  it("a remote worktree resolves remote attribution; a local one resolves local", () => {
    seedRemote();
    seedLocal();
    const remote = scopeAttributionForWorktree("rw1");
    expect(remote).toMatchObject({ scopeId: SCOPE, label: HOST, isRemote: true });
    const local = scopeAttributionForWorktree("lw1");
    expect(local).toMatchObject({ scopeId: LOCAL_SCOPE_ID, isRemote: false });
  });

  it("a remote project resolves remote attribution; a local one resolves local", () => {
    seedRemote();
    seedLocal();
    expect(scopeAttributionForProject("rp1")).toMatchObject({ label: HOST, isRemote: true });
    expect(scopeAttributionForProject("lp1")).toMatchObject({ isRemote: false });
  });
});
