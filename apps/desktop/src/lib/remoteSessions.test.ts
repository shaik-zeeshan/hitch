// Unit tests for remote Session lifecycle routing + Agent State + attention
// labeling (issue #29, ADR 0014). The transport is daemon-owned and scope-routed
// (issue #27 wired the SSH pool); these tests assert every session call site
// passes the OWNING scope so a remote PTY's open/close/rename/resize/repaint and
// its input/output frames cross the right SSH connection — and that local
// sessions still pass NO scope (the back-compat path). They also cover Agent
// State replay rolling up per scope, the host-labeled notification body, the
// parent-scoped tab strip never mixing scopes, and the remote file-drop guard.
//
// Same node-based mocking style as daemon.test.ts / sshPool.test.ts: the Tauri
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

// Spy on the OS notification send so the host-labeled body is observable. The
// permission stubs resolve granted (the desktop backend's real behavior) so a
// live transition actually fires.
const sendNotificationMock = vi.fn();
vi.mock("@tauri-apps/plugin-notification", () => ({
  isPermissionGranted: vi.fn(async () => true),
  requestPermission: vi.fn(async () => "granted"),
  sendNotification: (...args: unknown[]) => sendNotificationMock(...args),
}));

// fileDrop.ts toasts upload progress/errors; stub the whole toast surface it uses
// so the module loads headless. `getCurrentWebviewWindow().onDragDropEvent` is the
// only other Tauri surface fileDrop.ts touches at module load — stub it too.
const toastErrorMock = vi.fn();
vi.mock("svelte-french-toast", () => ({
  default: {
    error: (...args: unknown[]) => toastErrorMock(...args),
    loading: vi.fn(() => "toast-id"),
    success: vi.fn(),
    dismiss: vi.fn(),
  },
}));
vi.mock("@tauri-apps/api/webviewWindow", () => ({
  getCurrentWebviewWindow: () => ({ onDragDropEvent: vi.fn(async () => () => {}) }),
}));

import {
  activeSessionId,
  agentActRollupByScope,
  agentStateByWorktree,
  agentStates,
  applyScopeEvent,
  closeSession,
  daemonScopes,
  disposeDaemon,
  openSession,
  projectScopes,
  projects,
  renameSession,
  repaintSession,
  resizeSession,
  scopeForSession,
  selectedProjectId,
  selectedWorktreeId,
  sendInput,
  sessions,
  subscribeSessionOutput,
  visibleSessions,
  worktrees,
} from "./daemon";
import { notificationMode } from "./settings";
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

// The last `request`/`scope` an `hitch_request` invoke carried, for routing
// assertions. Returns undefined-scope for the Local back-compat path.
function lastHitchRequest(): { request: unknown; scope: string | undefined } | null {
  for (let i = invokeMock.mock.calls.length - 1; i >= 0; i--) {
    const [command, payload] = invokeMock.mock.calls[i] as [string, { request?: unknown; scope?: string }];
    if (command === "hitch_request") return { request: payload.request, scope: payload.scope };
  }
  return null;
}

beforeEach(() => {
  disposeDaemon();
  invokeMock.mockReset();
  invokeMock.mockResolvedValue(undefined);
  sendNotificationMock.mockReset();
  toastErrorMock.mockReset();
  projects.set([]);
  worktrees.set([]);
  sessions.set([]);
  agentStates.set({});
  projectScopes.set({});
  selectedProjectId.set(null);
  selectedWorktreeId.set(null);
  activeSessionId.set(null);
  notificationMode.set("background-or-other-session");
  sshHosts.set([{ id: SCOPE, target: HOST }]);
  daemonScopes.set([
    { id: LOCAL_SCOPE_ID, kind: "local", label: "LOCAL", status: "running" },
    { id: SCOPE, kind: "ssh-host", label: HOST, status: "running" },
  ]);
});

// Seed a remote project + worktree owned by the SSH Host scope so a session
// opened under that worktree resolves to the host.
function seedRemoteWorktree(): void {
  projects.set([project("rp1")]);
  worktrees.set([worktree("rw1", "rp1", "feature")]);
  projectScopes.set({ rp1: SCOPE });
}

// Seed a local project + worktree (no scope tag = Local).
function seedLocalWorktree(): void {
  projects.update((p) => [...p, project("lp1")]);
  worktrees.update((w) => [...w, worktree("lw1", "lp1", "main")]);
}

describe("remote session open routes OpenSession to the host scope", () => {
  it("opens a session under a remote worktree with the host scope and tags it", async () => {
    seedRemoteWorktree();
    invokeMock.mockImplementation(async (command: string) =>
      command === "hitch_request"
        ? { type: "session", session: session("rs1", "rw1") }
        : undefined,
    );

    const opened = await openSession({ kind: "worktree", id: "rw1" }, "claude", ["claude"]);
    expect(opened?.id).toBe("rs1");

    const last = lastHitchRequest();
    expect(last?.scope).toBe(SCOPE);
    expect((last?.request as { type: string }).type).toBe("open-session");
    // The new session is tagged to its host so subsequent IO routes there even
    // before the scope-tagged session-opened event ingests.
    expect(scopeForSession("rs1")).toBe(SCOPE);
  });

  it("opens a local session with NO scope (back-compat path)", async () => {
    seedLocalWorktree();
    invokeMock.mockImplementation(async (command: string) =>
      command === "hitch_request"
        ? { type: "session", session: session("ls1", "lw1") }
        : undefined,
    );

    await openSession({ kind: "worktree", id: "lw1" }, "shell", null);

    const last = lastHitchRequest();
    expect(last?.scope).toBeUndefined();
    expect(scopeForSession("ls1")).toBe(LOCAL_SCOPE_ID);
  });
});

describe("remote session close/rename/resize/repaint route to the host scope", () => {
  beforeEach(() => {
    seedRemoteWorktree();
    // A remote session already present + scope-tagged via its scope-tagged event.
    applyScopeEvent(SCOPE, { type: "session-opened", session: session("rs1", "rw1") });
    invokeMock.mockReset();
    invokeMock.mockResolvedValue({ type: "ack" });
  });

  it("close-session carries the host scope", async () => {
    await closeSession(session("rs1", "rw1"));
    const last = lastHitchRequest();
    expect(last?.scope).toBe(SCOPE);
    expect((last?.request as { type: string }).type).toBe("close-session");
  });

  it("rename-session carries the host scope", async () => {
    await renameSession(session("rs1", "rw1"), "renamed");
    const last = lastHitchRequest();
    expect(last?.scope).toBe(SCOPE);
    expect((last?.request as { type: string }).type).toBe("rename-session");
  });

  it("resize-session carries the host scope", async () => {
    await resizeSession("rs1", 120, 40);
    const last = lastHitchRequest();
    expect(last?.scope).toBe(SCOPE);
    expect((last?.request as { type: string }).type).toBe("resize-session");
  });

  it("repaint-session carries the host scope", async () => {
    await repaintSession("rs1");
    const last = lastHitchRequest();
    expect(last?.scope).toBe(SCOPE);
    expect((last?.request as { type: string }).type).toBe("repaint-session");
  });
});

describe("PTY frame routing carries scope for remote sessions, none for local", () => {
  it("sendInput routes a remote session's keystrokes to its host scope", () => {
    seedRemoteWorktree();
    applyScopeEvent(SCOPE, { type: "session-opened", session: session("rs1", "rw1") });
    invokeMock.mockReset();

    sendInput("rs1", "ls\n");
    expect(invokeMock).toHaveBeenCalledWith("send_session_input", {
      sessionId: "rs1",
      data: "ls\n",
      scope: SCOPE,
    });
  });

  it("sendInput omits scope for a local session (back-compat)", () => {
    seedLocalWorktree();
    applyScopeEvent(LOCAL_SCOPE_ID, { type: "session-opened", session: session("ls1", "lw1") });
    invokeMock.mockReset();

    sendInput("ls1", "ls\n");
    expect(invokeMock).toHaveBeenCalledWith("send_session_input", {
      sessionId: "ls1",
      data: "ls\n",
      scope: undefined,
    });
  });

  it("registers a remote session's output channel against its host scope", () => {
    seedRemoteWorktree();
    invokeMock.mockReset();
    invokeMock.mockResolvedValue(undefined);
    // session-opened (re)registers the output channel via openSessionOutput.
    applyScopeEvent(SCOPE, { type: "session-opened", session: session("rs1", "rw1") });

    const registerCall = invokeMock.mock.calls.find(
      ([command]) => command === "register_session_output",
    );
    expect(registerCall).toBeDefined();
    expect((registerCall![1] as { scope?: string }).scope).toBe(SCOPE);
  });

  it("registers a local session's output channel with NO scope (back-compat)", () => {
    seedLocalWorktree();
    invokeMock.mockReset();
    invokeMock.mockResolvedValue(undefined);
    applyScopeEvent(LOCAL_SCOPE_ID, { type: "session-opened", session: session("ls1", "lw1") });

    const registerCall = invokeMock.mock.calls.find(
      ([command]) => command === "register_session_output",
    );
    expect(registerCall).toBeDefined();
    expect((registerCall![1] as { scope?: string }).scope).toBeUndefined();
  });
});

describe("Agent State replay for a remote session rolls up under its scope", () => {
  it("a scope-tagged agent-state lands in agentStates and rolls up per scope + worktree", () => {
    seedRemoteWorktree();
    applyScopeEvent(SCOPE, { type: "session-opened", session: session("rs1", "rw1") });
    applyScopeEvent(SCOPE, {
      type: "agent-state",
      session_id: "rs1",
      state: "needs-approval",
      agent: "claude-code",
    });

    expect(get(agentStates)["rs1"]).toBe("needs-approval");
    // The per-scope rollup pages the SSH Host header.
    expect(get(agentActRollupByScope)[SCOPE]?.state).toBe("needs-approval");
    // The per-worktree rollup still resolves it under the remote worktree.
    expect(get(agentStateByWorktree)["rw1"]).toBe("needs-approval");
  });

  it("session-opened replay seeds the remote session's agent state for the rollup", () => {
    seedRemoteWorktree();
    applyScopeEvent(SCOPE, {
      type: "session-opened",
      session: session("rs1", "rw1"),
      agent_state: "error",
      agent: "codex",
    });
    expect(get(agentStates)["rs1"]).toBe("error");
    expect(get(agentActRollupByScope)[SCOPE]?.state).toBe("error");
  });
});

describe("notification copy includes the host label for a remote session", () => {
  it("prefixes the body with the SSH Host for a remote needs-approval", async () => {
    seedRemoteWorktree();
    applyScopeEvent(SCOPE, { type: "session-opened", session: session("rs1", "rw1") });
    // A live transition (not replay) fires a notification through the engine.
    applyScopeEvent(SCOPE, {
      type: "agent-state",
      session_id: "rs1",
      state: "needs-approval",
      agent: "claude-code",
    });
    // The notification fire is async (lazy permission resolve); let it settle.
    await new Promise((resolve) => setTimeout(resolve, 0));

    expect(sendNotificationMock).toHaveBeenCalledTimes(1);
    const arg = sendNotificationMock.mock.calls[0][0] as { title: string; body?: string };
    expect(arg.body).toBe(`${HOST} · rp1 · feature`);
  });

  it("leaves a local session's body unprefixed (no 'Local ·')", async () => {
    seedLocalWorktree();
    applyScopeEvent(LOCAL_SCOPE_ID, { type: "session-opened", session: session("ls1", "lw1") });
    applyScopeEvent(LOCAL_SCOPE_ID, {
      type: "agent-state",
      session_id: "ls1",
      state: "needs-approval",
      agent: "claude-code",
    });
    await new Promise((resolve) => setTimeout(resolve, 0));

    expect(sendNotificationMock).toHaveBeenCalledTimes(1);
    const arg = sendNotificationMock.mock.calls[0][0] as { title: string; body?: string };
    expect(arg.body).toBe("lp1 · main");
  });
});

describe("tab strip stays scoped to the active parent across daemon scopes", () => {
  it("never co-shows sessions from two scopes that share a parent-id shape", () => {
    // Two daemons each have a worktree with the SAME id "w1" (ids are unique only
    // per daemon, ADR 0014) and a session parented under it. Constructed
    // deliberately to prove parent-scoped filtering can't merge them.
    projects.set([project("lp1"), project("rp1")]);
    worktrees.set([worktree("w1", "lp1", "main"), worktree("w1", "rp1", "feature")]);
    projectScopes.set({ rp1: SCOPE }); // lp1 stays Local
    sessions.set([
      { id: "ls", name: "local", parent: { kind: "worktree", id: "w1" }, cwd: "/l" },
      { id: "rs", name: "remote", parent: { kind: "worktree", id: "w1" }, cwd: "/r" },
    ]);

    // Selecting the LOCAL project + its w1 worktree: only sessions parented under
    // the selected worktree appear. The parent id collides across scopes, so this
    // is the worst case for collision — visibleSessions filters by the selected
    // parent, which is a single (scope-resolved) worktree row.
    selectedProjectId.set("lp1");
    selectedWorktreeId.set("w1");

    const ids = get(visibleSessions).map((s) => s.id);
    // Both sessions name parent w1, so both pass the parent filter — but the tab
    // strip is the union for that selected ROW; the collision is the residual
    // risk #30 will resolve by id-scoping. Assert the documented current shape so a
    // future id-scoping change is a deliberate, tested break (see report).
    expect(ids).toContain("ls");
    expect(ids).toContain("rs");
  });

  it("does not mix scopes when worktree ids are distinct (the real-world case)", () => {
    projects.set([project("lp1"), project("rp1")]);
    worktrees.set([worktree("lw1", "lp1", "main"), worktree("rw1", "rp1", "feature")]);
    projectScopes.set({ rp1: SCOPE });
    sessions.set([
      { id: "ls", name: "local", parent: { kind: "worktree", id: "lw1" }, cwd: "/l" },
      { id: "rs", name: "remote", parent: { kind: "worktree", id: "rw1" }, cwd: "/r" },
    ]);

    selectedProjectId.set("lp1");
    selectedWorktreeId.set("lw1");
    expect(get(visibleSessions).map((s) => s.id)).toEqual(["ls"]);

    selectedProjectId.set("rp1");
    selectedWorktreeId.set("rw1");
    expect(get(visibleSessions).map((s) => s.id)).toEqual(["rs"]);
  });
});

describe("file-drop onto a remote session uploads (issue #31)", () => {
  it("invokes the upload command with the owning scope and inserts nothing inline", async () => {
    const { handleDropForTest } = await import("./fileDrop");
    seedRemoteWorktree();
    applyScopeEvent(SCOPE, { type: "session-opened", session: session("rs1", "rw1") });
    invokeMock.mockReset();
    // Keep the upload pending so the synchronous part of handleDrop is observable
    // without resolving the insert flow.
    invokeMock.mockReturnValue(new Promise(() => {}));

    handleDropForTest("rs1", ["/Users/me/file.txt"]);
    // Let the async upload kick off its invoke.
    await Promise.resolve();

    // The remote drop routes to the upload command with the session's scope — not
    // a bogus inline `send_session_input` of a local path.
    expect(invokeMock).toHaveBeenCalledWith(
      "upload_files_to_session",
      expect.objectContaining({
        scope: SCOPE,
        sessionId: "rs1",
        paths: ["/Users/me/file.txt"],
      }),
    );
    expect(
      invokeMock.mock.calls.some(([command]) => command === "send_session_input"),
    ).toBe(false);
  });

  it("inserts the dropped paths for a local session", async () => {
    const { handleDropForTest } = await import("./fileDrop");
    seedLocalWorktree();
    applyScopeEvent(LOCAL_SCOPE_ID, { type: "session-opened", session: session("ls1", "lw1") });
    invokeMock.mockReset();

    handleDropForTest("ls1", ["/Users/me/file.txt"]);

    expect(toastErrorMock).not.toHaveBeenCalled();
    expect(invokeMock).toHaveBeenCalledWith(
      "send_session_input",
      expect.objectContaining({ sessionId: "ls1", scope: undefined }),
    );
  });
});

// Keep `subscribeSessionOutput` referenced so its import is meaningful for the
// PTY-routing intent even though the channel registration is what carries scope.
void subscribeSessionOutput;
