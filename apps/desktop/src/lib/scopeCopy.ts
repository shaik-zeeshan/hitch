// Destructive-confirmation copy that names the owning daemon scope (issue #30,
// ADR 0014). These are PURE string builders — no store reads — so the destructive
// dialogs (remove worktree, remove project, discard) and their tests share one
// source of truth for "always include the SSH Host name and remote path so a path
// that also exists locally cannot be mistaken for local state" (ADR 0014).
//
// The Local scope keeps its existing clean copy (no "on Local", no host
// attribution); only a remote scope's copy carries the SSH Host label + remote
// path. The reactive scope resolution (which scope owns this worktree/project,
// what is its label) lives in daemon.ts; the dialog passes the resolved label and
// the entity's remote path in, so this module never touches a Svelte store.

import { LOCAL_SCOPE_ID, type DaemonScopeId } from "./types";

// The attribution a destructive dialog needs: the owning scope id, the SSH Host
// label for a remote scope (the target string, e.g. `prod`), and whether the
// scope is remote. A Local entity carries `isRemote: false` and the copy stays
// exactly as it was before issue #30.
export type ScopeAttribution = {
  scopeId: DaemonScopeId;
  label: string;
  isRemote: boolean;
};

// Whether a scope id is a remote SSH Host (anything that is not the Local scope).
export function isRemoteScopeId(scopeId: DaemonScopeId): boolean {
  return scopeId !== LOCAL_SCOPE_ID;
}

// Title for the remove-worktree dialog. Remote: `Remove worktree on <host>?`
// (ADR 0014's exact example). Local: the unchanged `Remove worktree`.
export function removeWorktreeTitle(attribution: ScopeAttribution): string {
  return attribution.isRemote
    ? `Remove worktree on ${attribution.label}?`
    : "Remove worktree";
}

// Title for the remove-project dialog. Remote: `Remove project on <host>?`;
// Local: the unchanged `Remove project`.
export function removeProjectTitle(attribution: ScopeAttribution): string {
  return attribution.isRemote
    ? `Remove project on ${attribution.label}?`
    : "Remove project";
}

// A one-line host + remote-path attribution rendered under a remote destructive
// dialog's title so the user sees WHICH machine and path the action touches (a
// path that also exists locally cannot be mistaken for local state; ADR 0014).
// Returns null for a Local entity, which renders no extra attribution line.
export function remotePathAttribution(
  attribution: ScopeAttribution,
  remotePath: string,
): string | null {
  if (!attribution.isRemote) return null;
  return `on ${attribution.label} · ${remotePath}`;
}

// The discard-changes confirm copy (a destructive `window.confirm`). Remote
// variants name the host so a discard on a remote worktree is never mistaken for
// the local one; local copy is unchanged.
export function discardFileConfirm(
  path: string,
  attribution: ScopeAttribution,
): string {
  return attribution.isRemote
    ? `Discard changes to ${path} on ${attribution.label}?`
    : `Discard changes to ${path}?`;
}

export function discardAllConfirm(
  count: number,
  attribution: ScopeAttribution,
): string {
  const files = `${count} changed file${count === 1 ? "" : "s"}`;
  return attribution.isRemote
    ? `Discard all ${files} on ${attribution.label}?`
    : `Discard all ${files}?`;
}
