// SSH Host configuration — GUI-local saved OpenSSH targets (issue #26, ADR 0014).
//
// An SSH Host is attachment configuration the GUI owns; it is NOT stored in any
// daemon's Project registry (CONTEXT.md). A saved host stores ONLY a trimmed
// OpenSSH target string — no private keys, passphrases, ports, or usernames as
// separate fields. OpenSSH config, ssh-agent, hardware keys, ProxyJump, and
// known_hosts remain the source of truth (ADR 0014).
//
// Persistence is localStorage (the same best-effort approach as settings.ts),
// kept here rather than settings.ts because it owns a richer shape (a list of
// objects) and its own validation/scope-seeding logic. The store seeds the
// daemon-scope tree at startup so saved hosts appear as top-level scope rows
// even before issue #27 adds the real proxy connection.
//
// ## Seam for issue #27
//
// `sshHostScope` mints each host's `DaemonScope` (kind "ssh-host", id
// `ssh:<target>`, label = target). Issue #27 reuses this id as the stable scope
// under which it interprets remote Projects/Worktrees/Sessions, and replaces the
// neutral `unreachable` status seeded here with the host's real Daemon Status
// once the proxy connection exists.

import { invoke } from "@tauri-apps/api/core";
import { get, writable } from "svelte/store";
import type { DaemonScope, DaemonScopeId, SshHost, SshTestResult } from "./types";

const SSH_HOSTS_KEY = "hitch.sshHosts";

// The well-known scope id a saved SSH Host mints. Stable across reloads (it is
// derived from the immutable target string), and namespaced with an `ssh:`
// prefix that can never collide with the reserved Local scope id (`local`).
export function sshHostId(target: string): DaemonScopeId {
  return `ssh:${target}`;
}

// Validate + normalize a user-entered OpenSSH target. Returns the trimmed target
// on success, or an error message explaining the rejection. Mirrors the Rust
// `normalize_target` rules so the dialog rejects the same inputs the backend
// would (and never sends an obviously-injectable target to `ssh`):
//  - trim surrounding whitespace; reject empty,
//  - reject embedded whitespace (a target is one host/alias token),
//  - reject a leading `-` (ssh would parse it as an option — injection).
export function validateTarget(raw: string): { ok: true; target: string } | { ok: false; error: string } {
  const trimmed = raw.trim();
  if (trimmed.length === 0) {
    return { ok: false, error: "Enter an SSH target (e.g. user@example.com or a host alias)." };
  }
  if (/\s/.test(trimmed)) {
    return { ok: false, error: "An SSH target cannot contain spaces." };
  }
  if (trimmed.startsWith("-")) {
    return { ok: false, error: "An SSH target cannot start with '-'." };
  }
  return { ok: true, target: trimmed };
}

function readStored(): SshHost[] {
  try {
    const raw = localStorage.getItem(SSH_HOSTS_KEY);
    if (!raw) return [];
    const parsed: unknown = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];
    // Defensive load: keep only well-formed entries with a valid target, and
    // re-derive the id from the target so a hand-edited/legacy store can't carry
    // a stale or colliding id. De-dupe by target (case-sensitive exact match).
    const seen = new Set<string>();
    const hosts: SshHost[] = [];
    for (const entry of parsed) {
      if (typeof entry !== "object" || entry === null) continue;
      const target = (entry as { target?: unknown }).target;
      if (typeof target !== "string") continue;
      const valid = validateTarget(target);
      if (!valid.ok || seen.has(valid.target)) continue;
      seen.add(valid.target);
      hosts.push({ id: sshHostId(valid.target), target: valid.target });
    }
    return hosts;
  } catch {
    // localStorage unavailable (SSR / private mode) or corrupt JSON: start empty.
    return [];
  }
}

// The persisted list of saved SSH Hosts, in alphabetical order by target (the
// tree's display order; ADR 0014 sorts SSH Hosts alphabetically). Writes persist
// best-effort to localStorage.
export const sshHosts = writable<SshHost[]>(readStored());

sshHosts.subscribe((hosts) => {
  try {
    localStorage.setItem(SSH_HOSTS_KEY, JSON.stringify(hosts));
  } catch {
    // Best-effort persistence (see settings.ts).
  }
});

// Whether a target (after trim) is already saved — case-sensitive exact match.
export function isDuplicateTarget(target: string): boolean {
  const trimmed = target.trim();
  return get(sshHosts).some((h) => h.target === trimmed);
}

// Add a validated, non-duplicate SSH Host, keeping the list alphabetical by
// target. Returns the saved host, or an error message (empty/invalid/duplicate)
// the dialog surfaces inline. Save does NOT require a passing test — the user may
// be offline — but invalid/duplicate targets are always rejected.
export function addSshHost(raw: string): { ok: true; host: SshHost } | { ok: false; error: string } {
  const valid = validateTarget(raw);
  if (!valid.ok) return valid;
  if (isDuplicateTarget(valid.target)) {
    return { ok: false, error: `“${valid.target}” is already saved.` };
  }
  const host: SshHost = { id: sshHostId(valid.target), target: valid.target };
  sshHosts.update((hosts) =>
    [...hosts, host].sort((a, b) => a.target.localeCompare(b.target)),
  );
  return { ok: true, host };
}

// Forget a saved SSH Host by id. Removing forgets ONLY the GUI-local entry; it
// does not (and in this slice cannot) touch any remote Daemon or its Sessions
// (ADR 0014). Issue #27's proxy disconnect hooks in here.
export function removeSshHost(id: DaemonScopeId): void {
  sshHosts.update((hosts) => hosts.filter((h) => h.id !== id));
}

// One saved SSH Host as a top-level daemon scope row (ADR 0014). Status is a
// neutral `unreachable` placeholder: no connection exists until issue #27, which
// replaces this with the host's real Daemon Status. The label is the target
// string verbatim (the tree shows it as the scope caption).
export function sshHostScope(host: SshHost): DaemonScope {
  return {
    id: host.id,
    kind: "ssh-host",
    label: host.target,
    status: "unreachable",
  };
}

// The saved SSH Hosts as daemon scopes, for seeding the tree. Ordering is left to
// `daemonScopesOrdered` (Local first, SSH Hosts alphabetically by label).
export function sshHostScopes(hosts: SshHost[]): DaemonScope[] {
  return hosts.map(sshHostScope);
}

// Run a Test Connection for a target via the backend. Spawns
// `ssh -o BatchMode=yes <target> hitch daemon proxy`, attempts the Hitch Hello
// handshake on its stdio, and returns a classified result. Validation errors are
// caught client-side first so an obviously-invalid target never reaches ssh.
export async function testSshHost(raw: string): Promise<SshTestResult> {
  const valid = validateTarget(raw);
  if (!valid.ok) {
    return { ok: false, category: "network", message: valid.error };
  }
  return invoke<SshTestResult>("test_ssh_host", { target: valid.target });
}
