// Unit tests for the FRONTEND half of the ssh-agent relay (proto v29, plan
// slices 4+6): the per-host `forwardAgent` toggle must ride every `set_ssh_hosts`
// reconcile entry, and the reconcile must only ever carry REMOTE SSH Hosts — the
// local scope is owned by `HitchClient` and never appears in the host list, which
// is the structural remote-only gate for the relay.
//
// Same node-based mocking style as sshPool.test.ts / sshHosts.test.ts: the Tauri
// `invoke`/`listen`/`Channel` surface is mocked so `daemon.ts` loads without a
// webview.
//
// NOTE (load-bearing): vitest can only see the structural gate here — that the
// flag is forwarded and the local scope is excluded. The ACTUAL `SshAgentRelay`
// prelude emission and the "is a local ssh-agent reachable?" gate are enforced
// Rust-side in `ssh_pool::connect_attempt` (it sends the prelude only when
// `forward_agent && ssh_agent_bridge::local_agent_socket().is_some()`), which
// this test cannot reach. See `ssh_agent_bridge.rs` for that side.

import { beforeEach, describe, expect, it, vi } from "vitest";

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

import { syncSshHostsToPool } from "./daemon";
import { LOCAL_SCOPE_ID, type SshHost } from "./types";

beforeEach(() => {
  invokeMock.mockReset();
  invokeMock.mockResolvedValue(undefined);
});

// Pull the `hosts` array out of the single `set_ssh_hosts` invoke.
function lastSetSshHostsArg(): Array<{ target: string; forwardAgent: boolean }> {
  const call = invokeMock.mock.calls.find(([cmd]) => cmd === "set_ssh_hosts");
  expect(call, "expected a set_ssh_hosts invoke").toBeDefined();
  return (call![1] as { hosts: Array<{ target: string; forwardAgent: boolean }> }).hosts;
}

describe("set_ssh_hosts carries the per-host forwardAgent flag", () => {
  it("forwards an explicit forwardAgent:true so the relay can be declared for that host", () => {
    const hosts: SshHost[] = [{ id: "ssh:prod", target: "prod", forwardAgent: true }];
    syncSshHostsToPool(hosts);
    expect(lastSetSshHostsArg()).toEqual([{ target: "prod", forwardAgent: true }]);
  });

  it("forwards forwardAgent:false so a user can opt a host out of the relay", () => {
    const hosts: SshHost[] = [{ id: "ssh:locked", target: "locked", forwardAgent: false }];
    syncSshHostsToPool(hosts);
    expect(lastSetSshHostsArg()).toEqual([{ target: "locked", forwardAgent: false }]);
  });

  it("defaults a legacy host (no forwardAgent field) to ON", () => {
    // Legacy persisted entries predate the toggle; they must read as relay-on so
    // existing hosts keep signing without a re-save.
    const hosts: SshHost[] = [{ id: "ssh:legacy", target: "legacy" }];
    syncSshHostsToPool(hosts);
    expect(lastSetSshHostsArg()).toEqual([{ target: "legacy", forwardAgent: true }]);
  });

  it("preserves each host's own flag across a mixed list", () => {
    const hosts: SshHost[] = [
      { id: "ssh:a", target: "a", forwardAgent: true },
      { id: "ssh:b", target: "b", forwardAgent: false },
      { id: "ssh:c", target: "c" },
    ];
    syncSshHostsToPool(hosts);
    expect(lastSetSshHostsArg()).toEqual([
      { target: "a", forwardAgent: true },
      { target: "b", forwardAgent: false },
      { target: "c", forwardAgent: true },
    ]);
  });
});

describe("the reconcile is structurally remote-only (the local scope never relays)", () => {
  it("never emits the local scope id as a host target", () => {
    // The local daemon is owned by HitchClient, not the pool; a relay is only ever
    // declared over a REMOTE connection. The host list the GUI reconciles to the
    // pool must therefore never contain the local scope.
    const hosts: SshHost[] = [
      { id: "ssh:prod", target: "prod", forwardAgent: true },
      { id: "ssh:dev", target: "dev", forwardAgent: false },
    ];
    syncSshHostsToPool(hosts);
    const targets = lastSetSshHostsArg().map((h) => h.target);
    expect(targets).not.toContain(LOCAL_SCOPE_ID);
    expect(targets).toEqual(["prod", "dev"]);
  });

  it("an empty host list reconciles to an empty pool (no local entry synthesized)", () => {
    syncSshHostsToPool([]);
    expect(lastSetSshHostsArg()).toEqual([]);
  });
});
