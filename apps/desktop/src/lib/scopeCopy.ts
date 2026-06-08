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

import { LOCAL_SCOPE_ID, type DaemonScopeId, type DaemonStatus } from "./types";

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

// ---- stale-scope liveness (issue #32, ADR 0014) ---------------------------
//
// A daemon scope is "live" (actionable) only while its Daemon Status is
// `running`. When an SSH Host is unreachable/failed — or mid-reconnect
// (`starting`) — daemon-backed actions in that scope are disabled and its last
// known tree is greyed as stale UI (ADR 0014: "marks the host unreachable, keeps
// the last tree greyed as stale UI, disables daemon-backed actions in that
// scope"). Greying whenever NOT running (rather than only on unreachable/failed)
// is deliberate: a scope in `starting` after a drop has no working PTY/request
// path either, so disabling its actions and greying its rows until it is
// genuinely `running` again avoids firing a daemon request at a half-open proxy.
//
// This is the single predicate every surface (tree greying, context-menu
// disabling, palette result/action gating, RightRail/Composer git actions) reads
// so the stale rule lives in exactly one place.
export function statusIsLive(status: DaemonStatus): boolean {
  return status === "running";
}

// Inverse of `statusIsLive`: whether a scope's last tree should render greyed as
// stale UI. Named for the tree/menu readers so call sites stay self-documenting.
export function statusIsStale(status: DaemonStatus): boolean {
  return !statusIsLive(status);
}

// ---- global-search scope metadata (issue #32, ADR 0014) -------------------
//
// Global search surfaces (the command palette) tag a remote Project/Worktree/
// Session result with its owning local/SSH Host scope as muted metadata in the
// host-first form `prod · project · branch` (ADR 0014's exact example), WITHOUT
// changing Session tab labels. Local results carry no scope prefix (the Local
// scope is the implicit default — no `Local ·` noise).
//
// `scopeMetadataPrefix` is the host segment a remote result prepends to its
// existing context; Local returns `null` (no prefix). Pure so the palette and its
// tests share one source of truth for the muted-metadata format.
export function scopeMetadataPrefix(attribution: ScopeAttribution): string | null {
  return attribution.isRemote ? attribution.label : null;
}

// ---- collapsed-host attention paging (issue #32, ADR 0014) ----------------
//
// A COLLAPSED SSH Host header must still page the user for `needs-approval` /
// `error` across its remote sessions (ADR 0014: "a collapsed host can still page
// for needs-approval or error"). The host row shows its attention rollup pill iff
// the host is collapsed AND it has an act-state rollup. Crucially, the pill shows
// even when the host is STALE (greyed/unreachable) — attention BEATS stale, so an
// approval that landed before the drop is never greyed away.
//
// Pure predicate so the component and tests share the rule. `hasRollup` is whether
// `agentActRollupByScope[scopeId]` resolved (an act state with a count); `expanded`
// is the host's tree expand state. Status is intentionally NOT an input: a stale
// host still pages.
export function showsCollapsedScopeRollup(opts: {
  hasRollup: boolean;
  expanded: boolean;
}): boolean {
  return opts.hasRollup && !opts.expanded;
}

// Join a scope's host prefix (if remote) ahead of an already-built context
// string with the palette's `·` separator, yielding `prod · project · branch`
// for a remote result and the unchanged context for a Local one. The trailing
// `context` is whatever the surface already shows (e.g. `project · branch`).
export function scopedSearchMetadata(
  attribution: ScopeAttribution,
  context: string,
): string {
  const prefix = scopeMetadataPrefix(attribution);
  return prefix ? `${prefix} · ${context}` : context;
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
