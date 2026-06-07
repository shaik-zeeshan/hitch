// Unit tests for the GUI-local SSH Host model (issue #26, ADR 0014): persistence
// round-trip, target validation (empty / whitespace / leading-dash), duplicate
// rejection, scope seeding (Local-first alphabetical), and removal.
//
// The Tauri `invoke` surface is mocked so the module loads without a webview, and
// a tiny localStorage shim makes persistence observable. Each test resets both.

import { beforeEach, describe, expect, it, vi } from "vitest";
import { get } from "svelte/store";

const invokeMock = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => invokeMock(...args),
}));

// In-memory localStorage shim (jsdom would also provide one, but pinning it here
// keeps the persistence assertions independent of the test environment).
const store = new Map<string, string>();
const localStorageMock = {
  getItem: (key: string) => store.get(key) ?? null,
  setItem: (key: string, value: string) => void store.set(key, value),
  removeItem: (key: string) => void store.delete(key),
  clear: () => store.clear(),
};
vi.stubGlobal("localStorage", localStorageMock);

const SSH_HOSTS_KEY = "hitch.sshHosts";

// The module reads localStorage at import time, so reset before importing.
async function freshModule() {
  vi.resetModules();
  return import("./sshHosts");
}

beforeEach(() => {
  store.clear();
  invokeMock.mockReset();
});

describe("validateTarget", () => {
  it("trims and accepts a plain alias and a user@host", async () => {
    const { validateTarget } = await freshModule();
    expect(validateTarget("  prod ")).toEqual({ ok: true, target: "prod" });
    expect(validateTarget("user@example.com")).toEqual({ ok: true, target: "user@example.com" });
  });

  it("rejects empty / whitespace-only", async () => {
    const { validateTarget } = await freshModule();
    expect(validateTarget("").ok).toBe(false);
    expect(validateTarget("   ").ok).toBe(false);
  });

  it("rejects embedded whitespace", async () => {
    const { validateTarget } = await freshModule();
    expect(validateTarget("host with space").ok).toBe(false);
  });

  it("rejects a leading dash (ssh option injection)", async () => {
    const { validateTarget } = await freshModule();
    expect(validateTarget("-oProxyCommand=evil").ok).toBe(false);
  });
});

describe("addSshHost + persistence", () => {
  it("adds a host and persists it to localStorage", async () => {
    const { addSshHost, sshHosts } = await freshModule();
    const result = addSshHost("  prod ");
    expect(result.ok).toBe(true);
    expect(get(sshHosts)).toEqual([{ id: "ssh:prod", target: "prod" }]);

    const persisted = JSON.parse(store.get(SSH_HOSTS_KEY)!);
    expect(persisted).toEqual([{ id: "ssh:prod", target: "prod" }]);
  });

  it("round-trips persisted hosts on reload", async () => {
    store.set(
      SSH_HOSTS_KEY,
      JSON.stringify([
        { id: "ssh:prod", target: "prod" },
        { id: "ssh:user@example.com", target: "user@example.com" },
      ]),
    );
    const { sshHosts } = await freshModule();
    expect(get(sshHosts).map((h) => h.target)).toEqual(["prod", "user@example.com"]);
  });

  it("keeps the list alphabetical by target", async () => {
    const { addSshHost, sshHosts } = await freshModule();
    addSshHost("zulu");
    addSshHost("alpha");
    addSshHost("mike");
    expect(get(sshHosts).map((h) => h.target)).toEqual(["alpha", "mike", "zulu"]);
  });

  it("rejects a duplicate target (exact match after trim)", async () => {
    const { addSshHost, isDuplicateTarget, sshHosts } = await freshModule();
    expect(addSshHost("prod").ok).toBe(true);
    expect(isDuplicateTarget("  prod ")).toBe(true);
    const dup = addSshHost(" prod ");
    expect(dup.ok).toBe(false);
    if (!dup.ok) expect(dup.error).toMatch(/already saved/);
    expect(get(sshHosts)).toHaveLength(1);
  });

  it("rejects an invalid target without saving", async () => {
    const { addSshHost, sshHosts } = await freshModule();
    expect(addSshHost("").ok).toBe(false);
    expect(addSshHost("  ").ok).toBe(false);
    expect(addSshHost("a b").ok).toBe(false);
    expect(addSshHost("-x").ok).toBe(false);
    expect(get(sshHosts)).toHaveLength(0);
  });

  it("drops malformed / duplicate entries when loading a hand-edited store", async () => {
    store.set(
      SSH_HOSTS_KEY,
      JSON.stringify([
        { target: "ok" },
        { target: "-evil" }, // invalid leading dash
        { target: "ok" }, // duplicate
        { nope: true }, // missing target
        "garbage", // not an object
      ]),
    );
    const { sshHosts } = await freshModule();
    expect(get(sshHosts)).toEqual([{ id: "ssh:ok", target: "ok" }]);
  });
});

describe("removeSshHost", () => {
  it("forgets only the targeted host and persists the removal", async () => {
    const { addSshHost, removeSshHost, sshHosts } = await freshModule();
    addSshHost("alpha");
    addSshHost("bravo");
    removeSshHost("ssh:alpha");
    expect(get(sshHosts).map((h) => h.target)).toEqual(["bravo"]);
    expect(JSON.parse(store.get(SSH_HOSTS_KEY)!)).toEqual([{ id: "ssh:bravo", target: "bravo" }]);
  });
});

describe("scope seeding", () => {
  it("mints an ssh-host scope per host with a neutral unreachable placeholder", async () => {
    const { sshHostScope, sshHostScopes } = await freshModule();
    const host = { id: "ssh:prod", target: "prod" };
    expect(sshHostScope(host)).toEqual({
      id: "ssh:prod",
      kind: "ssh-host",
      label: "prod",
      status: "unreachable",
    });
    const scopes = sshHostScopes([
      { id: "ssh:a", target: "a" },
      { id: "ssh:b", target: "b" },
    ]);
    expect(scopes.map((s) => s.id)).toEqual(["ssh:a", "ssh:b"]);
    expect(scopes.every((s) => s.kind === "ssh-host" && s.status === "unreachable")).toBe(true);
  });
});

describe("testSshHost", () => {
  it("invokes the backend command with the normalized target", async () => {
    invokeMock.mockResolvedValue({ ok: true, message: "Connected" });
    const { testSshHost } = await freshModule();
    const result = await testSshHost("  prod ");
    expect(invokeMock).toHaveBeenCalledWith("test_ssh_host", { target: "prod" });
    expect(result.ok).toBe(true);
  });

  it("short-circuits an invalid target without hitting the backend", async () => {
    const { testSshHost } = await freshModule();
    const result = await testSshHost("-x");
    expect(invokeMock).not.toHaveBeenCalled();
    expect(result.ok).toBe(false);
    expect(result.category).toBe("network");
  });
});
