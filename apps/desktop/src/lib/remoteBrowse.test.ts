// Unit tests for the remote directory browser + remote add/clone routing
// (issue #28, ADR 0014). Adding a Project inside an SSH Host scope browses that
// host's daemon (`list-directory` routed to its scope), then sends AddProject /
// CloneProject to that remote daemon; the returned/broadcast Project lands under
// the SSH Host scope. Same node-based mocking style as daemon.test.ts: the Tauri
// `invoke`/`listen`/`Channel` surface is mocked so the module loads headless.

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
// `addProject`/`cloneProject` call `pickAndAddProject` only on the local path; the
// remote flows under test never open the native picker. Stub the plugin anyway so
// importing daemon.ts is side-effect-free in node.
vi.mock("@tauri-apps/plugin-dialog", () => ({
  open: vi.fn(async () => null),
}));

import {
  applyHitchEvent,
  addProject,
  cloneProject,
  completeJob,
  daemonScopes,
  disposeDaemon,
  listDirectory,
  projectScopes,
  projects,
  projectsByScope,
  runJob,
  scopeForProject,
  selectedProjectId,
  sessions,
  worktrees,
} from "./daemon";
import { sshHosts } from "./sshHosts";
import { LOCAL_SCOPE_ID, type Project } from "./types";

const SCOPE = "ssh:prod";

const flush = () => new Promise((resolve) => setTimeout(resolve, 0));

const project = (id: string): Project => ({
  id,
  name: id,
  root: `/home/dev/${id}`,
  kind: "git-backed",
});

beforeEach(() => {
  disposeDaemon();
  invokeMock.mockReset();
  projects.set([]);
  worktrees.set([]);
  sessions.set([]);
  projectScopes.set({});
  selectedProjectId.set(null);
  sshHosts.set([{ id: SCOPE, target: "prod" }]);
  daemonScopes.set([
    { id: LOCAL_SCOPE_ID, kind: "local", label: "LOCAL", status: "running" },
    { id: SCOPE, kind: "ssh-host", label: "prod", status: "running" },
  ]);
});

describe("remote directory browser request routing", () => {
  it("routes a home-default listing to the host's scope (scope arg + null path)", async () => {
    invokeMock.mockResolvedValueOnce({
      type: "directory-listing",
      path: "/home/dev",
      parent: "/home",
      home: "/home/dev",
      entries: [{ name: "code", path: "/home/dev/code" }],
    });

    const listing = await listDirectory(null, false, SCOPE);

    expect(listing.path).toBe("/home/dev");
    expect(listing.entries).toEqual([{ name: "code", path: "/home/dev/code" }]);
    // The request is a `list-directory` carrying the scope so the SSH pool routes
    // it to that host's daemon (not the local one).
    expect(invokeMock).toHaveBeenCalledWith("hitch_request", {
      request: { type: "list-directory", path: null, show_hidden: false },
      scope: SCOPE,
    });
  });

  it("sends an explicit absolute path jump as the list-directory path", async () => {
    invokeMock.mockResolvedValueOnce({
      type: "directory-listing",
      path: "/srv/work",
      parent: "/srv",
      home: "/home/dev",
      entries: [],
    });

    await listDirectory("/srv/work", false, SCOPE);

    expect(invokeMock).toHaveBeenCalledWith("hitch_request", {
      request: { type: "list-directory", path: "/srv/work", show_hidden: false },
      scope: SCOPE,
    });
  });

  it("defaults hidden off and re-requests with show_hidden true on toggle", async () => {
    invokeMock.mockResolvedValue({
      type: "directory-listing",
      path: "/home/dev",
      parent: "/home",
      home: "/home/dev",
      entries: [],
    });

    // First load: the browser opens with hidden OFF.
    await listDirectory(null, false, SCOPE);
    expect(invokeMock).toHaveBeenLastCalledWith("hitch_request", {
      request: { type: "list-directory", path: null, show_hidden: false },
      scope: SCOPE,
    });

    // Toggling hidden re-requests the SAME directory with show_hidden true.
    await listDirectory("/home/dev", true, SCOPE);
    expect(invokeMock).toHaveBeenLastCalledWith("hitch_request", {
      request: { type: "list-directory", path: "/home/dev", show_hidden: true },
      scope: SCOPE,
    });
  });

  it("surfaces a daemon error so the browser can render an error row", async () => {
    invokeMock.mockResolvedValueOnce({
      type: "error",
      error: { code: "not-found", message: "cannot open /nope: not found", retryable: false },
    });

    await expect(listDirectory("/nope", false, SCOPE)).rejects.toThrow(
      "cannot open /nope: not found",
    );
  });
});

describe("adding an existing remote folder routes AddProject to the host scope", () => {
  it("sends add-project with the scope and homes the project under the host", async () => {
    // The remote AddProject reply (Projects) then the scoped refreshAll snapshot.
    const remoteProject = project("p-remote");
    invokeMock.mockImplementation(
      async (command: string, payload?: { request?: { type: string }; scope?: string }) => {
        if (command !== "hitch_request") return undefined;
        const type = payload?.request?.type;
        if (type === "add-project") return { type: "projects", projects: [remoteProject] };
        if (type === "list-projects") return { type: "projects", projects: [remoteProject] };
        if (type === "list-worktrees") return { type: "worktrees", worktrees: [] };
        if (type === "list-sessions") return { type: "sessions", sessions: [] };
        throw new Error(`unexpected request ${type}`);
      },
    );

    await addProject("/home/dev/code", SCOPE);

    // The add-project request carried the host scope.
    expect(invokeMock).toHaveBeenCalledWith("hitch_request", {
      request: { type: "add-project", root: "/home/dev/code" },
      scope: SCOPE,
    });
    // The resulting project is tagged to the host scope and shows under it.
    expect(scopeForProject("p-remote")).toBe(SCOPE);
    expect(get(projectsByScope)[SCOPE]?.map((p) => p.id)).toContain("p-remote");
  });

  it("a project broadcast on the host scope event stream lands under that host", () => {
    // The #27 event path tags a remote project-updated to its scope.
    applyHitchEvent({ type: "project-updated", project: project("p-evt") }, SCOPE);
    expect(scopeForProject("p-evt")).toBe(SCOPE);
    expect(get(projectsByScope)[SCOPE]?.map((p) => p.id)).toContain("p-evt");
  });

  it("a LOCAL add keeps its no-scope back-compat invoke", async () => {
    invokeMock.mockImplementation(
      async (command: string, payload?: { request?: { type: string } }) => {
        if (command !== "hitch_request") return undefined;
        const type = payload?.request?.type;
        if (type === "add-project") return { type: "projects", projects: [project("p-local")] };
        if (type === "list-projects") return { type: "projects", projects: [project("p-local")] };
        if (type === "list-worktrees") return { type: "worktrees", worktrees: [] };
        if (type === "list-sessions") return { type: "sessions", sessions: [] };
        throw new Error(`unexpected request ${type}`);
      },
    );

    await addProject("/local/repo");

    // Local omits the scope arg entirely (exact prior behavior).
    expect(invokeMock).toHaveBeenCalledWith("hitch_request", {
      request: { type: "add-project", root: "/local/repo" },
      scope: undefined,
    });
  });
});

describe("remote clone targets the chosen daemon scope", () => {
  it("starts the CloneProject Job on the host scope", async () => {
    invokeMock.mockImplementation(
      async (command: string, payload?: { request?: { type: string } }) => {
        if (command !== "hitch_request") return undefined;
        const type = payload?.request?.type;
        if (type === "start-job") return { type: "job-started", job_id: "j-clone" };
        if (type === "list-projects") return { type: "projects", projects: [] };
        if (type === "list-sessions") return { type: "sessions", sessions: [] };
        throw new Error(`unexpected request ${type}`);
      },
    );

    const done = cloneProject("git@host:owner/repo.git", "/home/dev/repo", null, SCOPE);
    await flush();

    // The StartJob wrapping CloneProject carried the host scope, so the Job runs
    // on that daemon and its lifecycle events flow back tagged to the scope.
    expect(invokeMock).toHaveBeenCalledWith("hitch_request", {
      request: {
        type: "start-job",
        request: {
          type: "clone-project",
          remote_url: "git@host:owner/repo.git",
          destination: "/home/dev/repo",
          name: null,
        },
      },
      scope: SCOPE,
    });

    // Resolve the Job so the awaiting clone settles (its refreshAll uses the
    // scoped snapshot mocked above).
    completeJob("j-clone", { type: "ack" });
    await done;
  });

  it("runJob without a scope keeps the local no-scope path", async () => {
    invokeMock.mockResolvedValueOnce({ type: "job-started", job_id: "j-local" });
    void runJob({ type: "fetch", worktree_id: "w1" }, "fetch");
    await flush();
    expect(invokeMock).toHaveBeenCalledWith("hitch_request", {
      request: { type: "start-job", request: { type: "fetch", worktree_id: "w1" } },
      scope: undefined,
    });
    completeJob("j-local", { type: "ack" });
  });
});
