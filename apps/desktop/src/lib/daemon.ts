// Data layer — the single owner of the daemon connection and the reactive app
// state behind the shell. Ported from the provisional React client (App.tsx);
// the request `type` strings, Tauri command names, and derived rollups are the
// contract — copied here, not redesigned. See docs/adr/0006-frontend-stack.md.
//
// State lives in Svelte stores so any component can read it with `$store` and
// the cross-cutting fix-up logic (selection fallbacks, stale-state cleanup)
// runs once here as store subscriptions rather than per-component effects.

import { Channel, invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import { derived, get, writable } from "svelte/store";
import { ByteRing } from "./byteRing";
import {
  aggregateActRollup,
  aggregateAgentState,
  parentKey,
  sessionBelongsTo,
  type ActRollup,
  type ActiveJobInfo,
  type AgentState,
  type BranchSummary,
  type ChangedFile,
  type CommitAndPushResult,
  type CompositeJobKind,
  type CompositeJobResult,
  type CompositeStep,
  type StepPhase,
  type CommitDiffRequest,
  type CommitDraft,
  type CommitFileDiff,
  type CommitInfo,
  type CommitMeta,
  type DaemonScope,
  type DaemonScopeId,
  type DaemonStatus,
  LOCAL_SCOPE_ID,
  type DraftGenerationSettings,
  type FileStatus,
  type GitStatus,
  type GitDiffRequest,
  type GitLogRequest,
  type HitchEvent,
  type Id,
  type JobStatus,
  type JobRequest,
  type KnownAgent,
  type PrFields,
  type PrInfo,
  isOpenPr,
  type PullRequestDraft,
  type Project,
  type RepaintSessionRequest,
  type Request,
  type Response,
  type StartJobRequest,
  type Session,
  type SessionParent,
  type Worktree,
} from "./types";
import {
  DEFAULT_DIFF_CONTEXT_LINES,
  diffContextLines,
  diffIgnoreWhitespace,
  draftClaudePath,
  draftCodexPath,
  draftCommitInstructions,
  draftModel,
  draftPrInstructions,
  draftProvider,
  railView,
  terminalFontFamily,
  terminalFontStack,
  type DraftProvider,
} from "./settings";
import {
  forgetSession as forgetSessionNotifications,
  noteAgentState,
  primeNotificationPermission,
} from "./notifications";

export type Connection = "connecting" | "ready" | "offline";

// ---- base stores ----------------------------------------------------------

export const connection = writable<Connection>("connecting");
export const error = writable<string | null>(null);

// ---- Daemon Status (ADR 0009) ---------------------------------------------
//
// The four-state liveness of the daemon process itself, pushed from src-tauri
// over `hitch-status`. `connection` (above) is the narrower per-window socket
// link, derived from this for the git-poll guard and the existing banner.
export const daemonStatus = writable<DaemonStatus>("starting");
export const daemonReason = writable<string | null>(null);
export const daemonLogPath = writable<string | null>(null);

// ---- Jobs (ADR 0008) ------------------------------------------------------
//
// Long-running daemon ops (clone/worktree creation, push/pull, PR, drafts)
// run as Jobs: the desktop sends `StartJob`, gets `JobStarted { job_id }`, and
// result arrives later in a `JobCompleted` event. This store mirrors live Jobs
// by id for quiet progress; `runJob` (below) bridges the event flow back to a
// Promise so callers keep their async API.
export type Job = {
  id: Id;
  status: JobStatus;
  message: string | null;
  kind: string | null;
  worktreeId: Id | null;
};
export const jobs = writable<Record<Id, Job>>({});

export const projects = writable<Project[]>([]);
export const worktrees = writable<Worktree[]>([]);
export const sessions = writable<Session[]>([]);

// Per-session hook-reported Agent State, the daemon-owned stored truth (ADR
// 0011). `waiting` lives here but renders unlabeled; `running` is gated to the
// WORKING display word by output activity (see `displaySessionStates`).
export const agentStates = writable<Record<Id, AgentState>>({});

// Per-session announced Agent identity (ADR 0011 amendment 2026-06-05): the
// agent's own `SessionStart` hook announces *which* agent the moment its TUI
// starts, replayed on attach via `SessionOpened`. This is the ONLY source for
// the Session mark's facepile/glyph — identity is never inferred from the
// session title or launch command. Cleared when the agent exits to `None`.
export const sessionAgents = writable<Record<Id, KnownAgent>>({});

// Per-session PTY output-activity gate (ADR 0011 amendment 2026-06-05): the
// daemon broadcasts edge-triggered `output-active` transitions (rising/falling)
// and seeds the current value in `SessionOpened`. `WORKING = running ∧
// output_active`; an interrupted/hung agent stops producing output, the gate
// falls, and the WORKING word drops to idle without the hook state changing.
export const sessionOutputActive = writable<Record<Id, boolean>>({});

// Per-session DISPLAY state the shell consumes (tabs, rollups). It applies two
// derivations to the stored hook state, both from the 2026-06-05 grill:
//  - `running` is gated by output activity: a session reporting `running` whose
//    output gate is closed renders as idle (the WORKING word is suppressed).
//  - `waiting` renders unlabeled, so it is omitted entirely.
// Act states (`needs-approval`, `error`) always pass through unchanged.
export const displaySessionStates = derived(
  [agentStates, sessionOutputActive],
  ([$agentStates, $outputActive]) => {
    const map: Record<Id, AgentState> = {};
    for (const [sessionId, state] of Object.entries($agentStates)) {
      if (state === "waiting") continue;
      if (state === "running" && !$outputActive[sessionId]) continue;
      map[sessionId] = state;
    }
    return map;
  },
);

// Live foreground command per session (the process the user is interacting
// with in the PTY), pushed by the daemon. Absent until the first report.
export const sessionCommands = writable<Record<Id, string | null>>({});
export const dirtyWorktrees = writable<Record<Id, boolean>>({});
export const worktreeLineStats = writable<Record<Id, { additions: number; deletions: number }>>({});

// ---- Daemon scopes (ADR 0014, issue #25) ----------------------------------
//
// The attached Daemons presented as top-level tree scopes. A GUI window may
// attach to several Daemons at once (CONTEXT.md); this slice ships only the
// local Daemon, but the state is modeled per scope so issue #27 can add SSH
// Host scopes additively. Every Project/Worktree/Session/Job id is interpreted
// within its owning scope (see `projectScope` below), never as globally unique
// across daemons (ADR 0014).
//
// The Local scope is always present, the daemon-status mirror keeps its `status`
// live, and `daemonScopesOrdered` renders Local first (then, later, SSH Hosts
// sorted by target). Seeded with Local at module load so the tree always has its
// top-level scope even before the first connect.
const LOCAL_SCOPE: DaemonScope = {
  id: LOCAL_SCOPE_ID,
  kind: "local",
  label: "LOCAL",
  status: "starting",
};
export const daemonScopes = writable<DaemonScope[]>([LOCAL_SCOPE]);

// The attached scopes in tree order: Local first, then SSH Hosts alphabetically
// by label (ADR 0014). Local always sorts ahead of any remote scope regardless
// of label; remote scopes (issue #27) order among themselves by target string.
export const daemonScopesOrdered = derived(daemonScopes, ($scopes) =>
  [...$scopes].sort((a, b) => {
    if (a.kind !== b.kind) return a.kind === "local" ? -1 : 1;
    if (a.id === LOCAL_SCOPE_ID) return -1;
    if (b.id === LOCAL_SCOPE_ID) return 1;
    return a.label.localeCompare(b.label);
  }),
);

// Which daemon scope owns each Project, by project id. The owning scope is
// fixed at ingestion: a Project reached over the local socket belongs to the
// Local scope; a Remote Project (issue #28) will be tagged with its SSH Host
// scope when it is ingested from that daemon. Worktrees/Sessions inherit their
// Project's scope (they are reached through the same daemon), so this one map is
// the single source of scope membership for the whole entity tree. Absent =
// not yet ingested; consumers fall back to Local (the only scope today).
export const projectScopes = writable<Record<Id, DaemonScopeId>>({});

// The owning daemon scope of a Project id, defaulting to Local for any project
// not explicitly tagged (the only scope this slice ingests). The single helper
// every scope-aware reader uses so the Local default lives in one place.
export function scopeForProject(projectId: Id | null | undefined): DaemonScopeId {
  if (!projectId) return LOCAL_SCOPE_ID;
  return get(projectScopes)[projectId] ?? LOCAL_SCOPE_ID;
}

// Tag a set of Projects as owned by a scope at ingestion. Replaces the scope map
// wholesale for a full snapshot (so a project that vanished loses its tag);
// `mergeProjectScopes` keeps existing tags for incremental upserts.
function setProjectScopes(projectIds: Id[], scopeId: DaemonScopeId): void {
  projectScopes.set(Object.fromEntries(projectIds.map((id) => [id, scopeId])));
}

function tagProjectScope(projectId: Id, scopeId: DaemonScopeId): void {
  projectScopes.update((current) =>
    current[projectId] === scopeId ? current : { ...current, [projectId]: scopeId },
  );
}

export const selectedProjectId = writable<Id | null>(null);
export const selectedWorktreeId = writable<Id | null>(null);
export const activeSessionId = writable<Id | null>(null);

// The daemon scope the current selection lives in (ADR 0014 scope-aware
// selection): a Project/Worktree/Session selection belongs to the scope that
// owns its Project. Derived off the selected project so it can never drift from
// the selection; defaults to Local when nothing is selected (the scope a fresh
// add-project / palette action targets). When SSH Host scopes exist (issue #27)
// this is how global actions resolve their target daemon.
export const selectedScopeId = derived(
  [selectedProjectId, projectScopes],
  ([$projectId, $scopes]) => (($projectId && $scopes[$projectId]) || LOCAL_SCOPE_ID),
);

// Git view state (consumed by the Changes panel + diff tabs).
export const gitStatus = writable<GitStatus | null>(null);

// One open diff per changed file the user has clicked. Each tab carries its own
// fetched text (`null` while loading / for the binary/empty cases). Tabs persist
// as peers of the session tabs in the strip; clicking a file that's already open
// re-activates its tab rather than spawning a duplicate. Paths are worktree-
// relative, so the set is cleared wholesale on worktree switch.
// `staged` records which side a single-file tab was opened with, so a re-diff
// (e.g. after toggling ignore-whitespace) can re-fetch the same git target it
// originally showed. The all-changes sentinel tab carries no staged side (its
// per-file rows track their own); undefined keeps the daemon's legacy selection.
export type DiffTabEntry = { path: string; text: string | null; staged?: boolean };
export const diffTabs = writable<DiffTabEntry[]>([]);
// Sentinel "path" for the single all-changes tab — the unified view that shows
// every changed file at once, each as its own collapsible section. The NUL byte
// can never appear in a real worktree-relative path, so this never collides with
// a file. Its tab carries no `text` (its content is `allChangesFiles`, not a
// single diff string); the back-compat `diffText`/`parseDiff` path skips it.
export const ALL_CHANGES_TAB = "\0all";

// Sentinel "path" prefix for a Commit Tab — one diff tab per commit, keyed by
// sha (`\0commit:<sha>`). Like `ALL_CHANGES_TAB`, the leading NUL byte can never
// appear in a real worktree-relative path, so a commit tab never collides with a
// file or with the all-changes sentinel. Its `text` is unused (its content is the
// per-sha commit diff cache, not a single diff string); the single-file
// `diffText`/`parseDiff` path skips it like the all-changes tab.
export const COMMIT_TAB_PREFIX = "\0commit:";
// The sentinel tab path for a commit sha.
export function commitTabPath(sha: string): string {
  return `${COMMIT_TAB_PREFIX}${sha}`;
}
// Whether a diff-tab path is a Commit Tab sentinel.
export function isCommitTab(path: string): boolean {
  return path.startsWith(COMMIT_TAB_PREFIX);
}
// The full sha carried by a Commit Tab sentinel path, or `null` if it isn't one.
export function commitShaFromTab(path: string): string | null {
  return isCommitTab(path) ? path.slice(COMMIT_TAB_PREFIX.length) : null;
}

// Per-file diffs for the all-changes view, in the order RightRail lists them
// (staged first, then unstaged). `text` is `null` while that file's `git-diff`
// fetch is in flight. Populated by `viewAllChanges`; the view component renders
// one @pierre/diffs instance per expanded entry from these.
export type AllChangesFile = { path: string; staged: boolean; text: string | null };
export const allChangesFiles = writable<AllChangesFile[]>([]);
// Which all-changes rows are currently expanded, keyed by `allChangesRowKey`
// (side + path, since a partially-staged file appears as two rows). DiffAllTab
// owns the toggling; the daemon reads it so a refresh only fetches diffs for
// rows the user actually has expanded. A key absent from the set means collapsed
// — the default — so an initial open (and any row that newly appears in status)
// starts collapsed and fetches nothing until the user expands it, individually
// or via the head's expand-all toggle.
export const allChangesExpanded = writable<Set<string>>(new Set());
// Row identity for the all-changes view: side + path. A partially-staged file
// contributes a staged and an unstaged row that diff differently, so the side
// has to be part of the key. Shared with DiffAllTab (which keys its sections the
// same way) through this exported helper so the two never drift.
export function allChangesRowKey(path: string, staged: boolean): string {
  return `${staged ? "staged" : "unstaged"}\0${path}`;
}
const allChangesRowExpanded = (path: string, staged: boolean): boolean =>
  get(allChangesExpanded).has(allChangesRowKey(path, staged));
// Keep the daemon/webview responsive for large working trees: All changes may
// contain hundreds of rows, and untracked directory rows can expand to large
// diffs. Fetch a small pool instead of starting one git-diff per row at once.
const ALL_CHANGES_DIFF_CONCURRENCY = 4;
// The path of the diff tab currently shown when the diff view is active. `null`
// when no diff tabs are open.
export const activeDiffPath = writable<string | null>(null);
// Whether a diff tab is the active center view. Diff tabs persist as peers of
// the session tabs (so they can be open while a terminal shows); this flag is
// what the tab bar and center pane switch on.
export const diffActive = writable<boolean>(false);

// Active-tab projection kept for the diff renderer + Changes-panel highlight,
// which only ever care about the visible diff. It derives off the tab set so
// there's a single source of truth (`diffTabs` + `activeDiffPath`).
// Which side the visible diff tab shows. A partially-staged file appears as two
// Changes-panel rows (staged + unstaged), so the highlight must compare both
// axes; `undefined` (legacy/no explicit side) matches either row.
export const diffStaged = derived(
  [diffTabs, activeDiffPath],
  ([$tabs, $path]) => $tabs.find((tab) => tab.path === $path)?.staged,
);
export const diffText = derived(
  [diffTabs, activeDiffPath],
  ([$tabs, $path]) => $tabs.find((tab) => tab.path === $path)?.text ?? null,
);
export const gitBusy = writable<boolean>(false);
export const prUrl = writable<string | null>(null);
// The PR (if any) GitHub has for the selected worktree's branch. `null` = none
// known (or not yet checked). This preserves closed/merged metadata for display;
// use `openPrInfo` when the action state machine needs an actually-open PR.
export const prInfo = writable<PrInfo | null>(null);
export const openPrInfo = derived(prInfo, ($pr) => (isOpenPr($pr) ? $pr : null));
// PR-per-worktree, so the sidebar can show a PR chip on each branch without each
// row firing its own lookup. Populated as a side effect of `loadPrStatus` (which
// already runs on worktree switch + after git ops), so a chip appears once a
// worktree's status has been fetched at least once. A missing key = not yet
// known (no chip); an explicit `null` = checked, no PR.
export const prByWorktree = writable<Record<Id, PrInfo | null>>({});

// Monotonic freshness clock shared by every path that writes `prByWorktree`
// (per-worktree `loadPrStatus` and the batched `loadProjectPrStatuses`). Each
// request stamps a seq at start; an entry is only overwritten by a seq at least
// as new as the one last applied to it, so a slow project-wide response can't
// regress a worktree whose per-worktree status was refreshed more recently
// (e.g. right after creating a PR).
let prByWorktreeSeq = 0;
const prByWorktreeApplied = new Map<Id, number>();
// Highest seq for which a lookup has *started* (not necessarily completed) per
// worktree. `prByWorktreeApplied` only advances on completed writes, so without
// this an older project-wide response that resolves while a newer per-worktree
// lookup is still in flight would apply stale data — and, if that fresher lookup
// then fails, leave the chip wrong until the next poll. Stamping the started seq
// lets the freshness guard reject any response older than an in-flight request.
const prByWorktreeStarted = new Map<Id, number>();
// The freshest seq known for a worktree, whether a lookup merely started or has
// fully landed. A write/clear is only fresh if its seq is >= this.
function freshestPrSeq(worktreeId: Id): number {
  return Math.max(
    prByWorktreeApplied.get(worktreeId) ?? 0,
    prByWorktreeStarted.get(worktreeId) ?? 0,
  );
}
// Returns whether this write was the freshest seen for `worktreeId` (i.e. it
// actually applied). Callers gate the selected worktree's single `prInfo` on the
// result so an unrelated project's batched lookup can't drop a newer per-worktree
// result through a shared global counter.
function writePrByWorktree(worktreeId: Id, pr: PrInfo | null, seq: number): boolean {
  if (seq < freshestPrSeq(worktreeId)) return false;
  prByWorktreeApplied.set(worktreeId, seq);
  prByWorktree.update((map) => ({ ...map, [worktreeId]: pr }));
  return true;
}

// Test-only inspector: whether either PR-freshness map still holds bookkeeping
// for a worktree. These maps are otherwise module-private (their monotonic seqs
// have no public-store effect), so this lets the leak tests assert that a removed
// worktree's entries are swept rather than retained until disposeDaemon.
export function __prFreshnessTracked(worktreeId: Id): boolean {
  return prByWorktreeApplied.has(worktreeId) || prByWorktreeStarted.has(worktreeId);
}

const diffCache = new Map<string, string>();
// Diff freshness is scoped by consumer. A single-file tab and the all-changes
// fan-out may request the same path concurrently; each should only gate its own
// UI state, while cache writes are guarded by a separate per-path clock below.
const diffRequestSeq = new Map<string, number>();
const diffCacheWriteSeq = new Map<string, number>();
const diffSideKey = (staged: boolean | undefined) =>
  staged === undefined ? "legacy" : staged ? "staged" : "worktree";

// The two re-diff options change the diff *text* the daemon returns, so they're
// part of the cache identity: toggling either must not serve a diff fetched
// under the old options. Read live from the settings stores so every cache key
// (and every request) reflects the user's current choice. Defaults serialize as
// the omitted shape, matching what the request builder sends.
function diffOptionFields(): { ignore_whitespace?: boolean; context_lines?: number } {
  const fields: { ignore_whitespace?: boolean; context_lines?: number } = {};
  if (get(diffIgnoreWhitespace)) fields.ignore_whitespace = true;
  const context = get(diffContextLines);
  if (context !== DEFAULT_DIFF_CONTEXT_LINES) fields.context_lines = context;
  return fields;
}
// Stable string for the cache key: default options collapse to "" so warm
// caches from before any toggle still hit at default settings.
function diffOptionKey(): string {
  const fields = diffOptionFields();
  if (fields.ignore_whitespace === undefined && fields.context_lines === undefined) return "";
  return `iw${fields.ignore_whitespace ? 1 : 0}\0cx${fields.context_lines ?? DEFAULT_DIFF_CONTEXT_LINES}`;
}
const diffCacheKey = (worktreeId: Id, path: string, staged?: boolean) =>
  `${worktreeId}\0${diffSideKey(staged)}\0${diffOptionKey()}\0${path}`;
const diffFreshnessKey = (worktreeId: Id, consumer: string, path: string, staged?: boolean) =>
  `${worktreeId}\0${consumer}\0${diffSideKey(staged)}\0${path}`;
const diffTabFreshnessKey = (worktreeId: Id, path: string) =>
  diffFreshnessKey(worktreeId, "single", path);

function nextDiffRequestSeq(
  worktreeId: Id,
  consumer: string,
  path: string,
  staged?: boolean,
): number {
  const requestKey = diffFreshnessKey(worktreeId, consumer, path, staged);
  const requestSeq = (diffRequestSeq.get(requestKey) ?? 0) + 1;
  diffRequestSeq.set(requestKey, requestSeq);
  return requestSeq;
}

function nextDiffCacheWriteSeq(cacheKey: string): number {
  const writeSeq = (diffCacheWriteSeq.get(cacheKey) ?? 0) + 1;
  diffCacheWriteSeq.set(cacheKey, writeSeq);
  return writeSeq;
}

function writeFreshDiffCache(cacheKey: string, writeSeq: number, text: string): boolean {
  if (diffCacheWriteSeq.get(cacheKey) !== writeSeq) return false;
  diffCache.set(cacheKey, text);
  return true;
}

// Worktrees whose open diffs (all-changes AND single-file tabs) must refresh on
// the next git status, even when the status metadata is byte-identical. A
// worktree-dirty event sets this because an external edit can change the diff
// text while line counts / file set stay the same (see the gitStatus subscriber).
const forcedDiffRefreshWorktrees = new Set<Id>();

// Drop every cached diff variant for one worktree/path/side, across ALL re-diff
// option keys — not just the one active now. The option key sits between the side
// token and the path in the cache key (`worktree\0side\0optionKey\0path`), so a
// fixed-key delete would only evict the current-option variant and leave a stale
// entry under another option key (e.g. ignore-whitespace toggled) that a later
// toggle-back would serve without re-asking the daemon. The `\0` delimiter bounds
// every segment, so matching the worktree+side prefix and the `\0path` suffix is
// unambiguous (no false positives between paths that share a suffix, or between
// sides whose names share a prefix). `staged === undefined` invalidates all sides.
//
// Bumping the per-key write-seq is as load-bearing as the delete: an in-flight
// `git-diff` captured the current write-seq before this invalidation and would
// otherwise pass `writeFreshDiffCache`'s seq check and repopulate the just-cleared
// entry with its now-stale text. That refill is invisible until the cache is read
// again — and the "All changes" collapse-during-refresh path issues no follow-up
// fetch to bump the seq, so expanding the row later serves the pre-change diff.
// Bumping here invalidates any request older than this point. The write-seq map is
// the superset (every cache write goes through `nextDiffCacheWriteSeq`, plus
// in-flight keys not yet written), so iterating it covers both the delete and the
// bump, including requests whose entry was never cached.
export function invalidateDiffCacheVariants(worktreeId: Id, path: string, staged?: boolean): void {
  const pathSuffix = `\0${path}`;
  const prefix =
    staged === undefined ? `${worktreeId}\0` : `${worktreeId}\0${diffSideKey(staged)}\0`;
  for (const key of diffCacheWriteSeq.keys()) {
    if (key.startsWith(prefix) && key.endsWith(pathSuffix)) {
      diffCache.delete(key);
      diffCacheWriteSeq.set(key, (diffCacheWriteSeq.get(key) ?? 0) + 1);
    }
  }
}

function deleteDiffCacheForChangedFiles(worktreeId: Id, files: ChangedFile[]): void {
  for (const file of files) invalidateDiffCacheVariants(worktreeId, file.path, file.staged);
}

// Arm a one-shot forced refresh for the worktree if it's selected and has any
// open diff that tracks the working tree — the all-changes tab OR a single-file
// (non-commit) tab. Commit tabs are immutable per-sha snapshots and never need
// this. The flag is consumed by the gitStatus subscriber on the next status.
function requestDiffRefreshOnNextStatus(worktreeId: Id): void {
  if (get(gitWorktreeId) !== worktreeId) return;
  const tracksWorkingTree = get(diffTabs).some(
    (tab) => tab.path === ALL_CHANGES_TAB || !isCommitTab(tab.path),
  );
  if (tracksWorkingTree) forcedDiffRefreshWorktrees.add(worktreeId);
}

// ---- History commit log ---------------------------------------------------
//
// The HISTORY rail view's commit log for the selected worktree: paginated, fetched
// lazily a page at a time. `commits` is the accumulated pages in order (newest
// first); `hasMore` flags more past the last fetched page; `loading` is true while
// any page request is in flight. `worktreeId` records which worktree the rows
// belong to so a stale fetch from a previous worktree can't land into the new one,
// mirroring the seq guards on `loadGitStatus`/diffs. The store swaps wholesale on
// worktree switch (the gitWorktreeId subscription resets it and refetches page one).
export type CommitLogState = {
  worktreeId: Id | null;
  commits: CommitInfo[];
  hasMore: boolean;
  loading: boolean;
  // How many commits a HEAD-move refresh just PREPENDED above the existing rows.
  // The History list reads this to compensate scrollTop by the inserted rows'
  // height so the viewport stays anchored to the same commit (rather than the new
  // top commits sliding every visible row down under a churning HEAD). `tick` is a
  // monotonic stamp so the component re-anchors once per prepend even when two
  // refreshes prepend the same count back-to-back.
  prependedCount: number;
  tick: number;
};
const EMPTY_COMMIT_LOG: CommitLogState = {
  worktreeId: null,
  commits: [],
  hasMore: false,
  loading: false,
  prependedCount: 0,
  tick: 0,
};
export const commitLog = writable<CommitLogState>(EMPTY_COMMIT_LOG);
// Page size for each `git-log` fetch (lazy "load more" pulls the next page).
const COMMIT_LOG_PAGE = 20;
// Monotonic freshness clock: a slow page response from a previous worktree (or a
// superseded reset) must not land. Each load stamps a seq; only the freshest
// applies, mirroring `statusRequestSeq`.
let commitLogSeq = 0;
// The HEAD sha the current log was last fetched for, so the status backbone can
// detect a HEAD change and refetch only when it actually moved.
let commitLogHeadId: string | null = null;

// How a `loadCommitLogPage` call treats the rows it already holds:
//   - "reset":   replace from page one, clearing the rows first (worktree switch
//                / first open) — the array genuinely belongs to a new context.
//   - "append":  add the next offset page at the bottom (loadMore).
//   - "refresh": a HEAD moved under the SAME worktree (e.g. an agent committed in
//                the selected PTY). Fetch the fresh top window, then PREPEND only
//                the genuinely-new commits above the rows we already hold —
//                keeping every existing row's object identity (so the sha-keyed
//                `{#each … (commit.id)}` does zero DOM work for them) and the full
//                paginated depth (new commits GROW the list rather than pushing
//                the oldest rows out of a fixed window). The component compensates
//                scrollTop for the prepended height so the viewport stays anchored
//                to the same commit instead of drifting down under a churning
//                HEAD. This is the fix for the "late row click does nothing /
//                snaps back" bug: the old refresh swapped the WHOLE window every
//                ~1s while an agent committed, so visible rows slid down by a row
//                each tick (scrollTop never compensated) — a real pointer's
//                mousedown and mouseup then landed on DIFFERENT rows and the
//                browser synthesized NO `click`, and the oldest rows silently fell
//                out of the window. A prepend-only, scroll-anchored refresh fixes
//                both. When the new commits don't overlap our top sha within the
//                fetched window (a big jump, a rebase, or a force-update), we fall
//                back to replacing the window — correctness over node reuse.
type CommitLogLoad = "reset" | "append" | "refresh";

// Fetch commit-log rows for the selected worktree under one of the modes above.
// A no-op if no git worktree is selected.
async function loadCommitLogPage(mode: CommitLogLoad): Promise<void> {
  const worktreeId = get(gitWorktreeId);
  if (!worktreeId) return;
  const current = get(commitLog);
  const sameWorktree = current.worktreeId === worktreeId;
  // A refresh of a worktree we don't actually hold yet is just a first load.
  const effective: CommitLogLoad = mode === "refresh" && !sameWorktree ? "reset" : mode;

  // Append starts past the rows we hold; reset/refresh start at the top. Refresh
  // fetches only ONE page from the top, not the full loaded depth: the merge below
  // keeps just the new prefix above our top sha and reuses the rest of our rows
  // verbatim, so fetching the whole depth made the daemon recompute a per-commit
  // tree diff (`commit_line_totals`) for every loaded row just to discard all but
  // the prefix — O(loaded) wasted work on EVERY HEAD move (an agent committing in
  // a 200-row History paid ~200 redundant diffs to deliver one prepended row). One
  // page covers the common case (≤ PAGE new commits since the last refresh); the
  // overlap is within it. A burst of > PAGE fast-forward commits won't overlap in
  // one page — that's handled by a bounded one-step escalation in the merge below
  // (refetch once at the old full depth) so a pure fast-forward keeps the user's
  // scrolled depth; a genuine rewrite still falls through to the full replace.
  const offset = effective === "append" && sameWorktree ? current.commits.length : 0;
  const limit = COMMIT_LOG_PAGE;
  if (effective === "append" && sameWorktree && !current.hasMore) return;
  // A refresh whose HEAD didn't actually change nothing-to-do early-out is the
  // caller's job (it only refreshes on a HEAD move); but if we somehow hold no
  // rows yet, treat it as a first load so the window is established.
  const refreshHasRows = effective === "refresh" && current.commits.length > 0;

  const seq = ++commitLogSeq;
  commitLog.update((state) =>
    effective === "reset" || state.worktreeId !== worktreeId
      ? { ...state, worktreeId, commits: [], hasMore: false, loading: true, prependedCount: 0 }
      : // append + refresh keep the rows in place (the merge below swaps/prepends
        // only once the fresh page lands, so no node churn while it's in flight).
        { ...state, loading: true, prependedCount: 0 },
  );

  // The fetch is still the freshest and its worktree is still selected.
  const isLatest = () => commitLogSeq === seq && get(gitWorktreeId) === worktreeId;

  const fetchLog = (fetchLimit: number) =>
    daemonRequest<Response & { commits: CommitInfo[]; has_more: boolean }>({
      type: "git-log",
      worktree_id: worktreeId,
      limit: fetchLimit,
      offset,
    } satisfies GitLogRequest);

  try {
    const response = await fetchLog(limit);
    if (!isLatest()) return;

    if (effective === "append") {
      commitLog.update((state) => ({
        ...state,
        worktreeId,
        commits: [...state.commits, ...response.commits],
        hasMore: response.has_more,
        loading: false,
        prependedCount: 0,
      }));
      return;
    }

    if (refreshHasRows) {
      // The fresh window is newest-first; the commits ABOVE our current top sha are
      // genuinely new. Prepend only those, reusing the EXISTING objects for the rest
      // (so the sha-keyed each does zero DOM work for unchanged rows) and GROWING the
      // list rather than dropping the oldest loaded rows to a fixed window.
      let merged = mergeRefresh(get(commitLog), response.commits, worktreeId);
      // No overlap in one page: either a fast-forward burst landed > PAGE commits on
      // top (the old top sha exists, just deeper than one page) or history was
      // rewritten (the old sha is gone). Escalate ONCE at the previous full depth so
      // a pure fast-forward recovers the overlap and keeps the user's scrolled depth.
      // If the second window still has no overlap it's a real rewrite → full replace.
      if (merged === NO_OVERLAP && current.commits.length > COMMIT_LOG_PAGE) {
        const deep = await fetchLog(current.commits.length);
        if (!isLatest()) return;
        merged = mergeRefresh(get(commitLog), deep.commits, worktreeId);
        if (merged === NO_OVERLAP) {
          // Genuine rewrite: replace the window (the loaded rows may be gone).
          commitLog.update((state) => ({
            ...state,
            worktreeId,
            commits: deep.commits,
            hasMore: deep.has_more,
            loading: false,
            prependedCount: 0,
          }));
          return;
        }
      } else if (merged === NO_OVERLAP) {
        // Only one page was ever loaded and it shares no sha — a rewrite. Replace.
        commitLog.update((state) => ({
          ...state,
          worktreeId,
          commits: response.commits,
          hasMore: response.has_more,
          loading: false,
          prependedCount: 0,
        }));
        return;
      }
      commitLog.update(() => merged as CommitLogState);
      return;
    }

    // reset (or a refresh with no prior rows): take the fresh window as-is.
    commitLog.update((state) => ({
      ...state,
      worktreeId,
      commits: response.commits,
      hasMore: response.has_more,
      loading: false,
      prependedCount: 0,
    }));
  } catch (err) {
    error.set(toMessage(err));
    if (isLatest()) commitLog.update((state) => ({ ...state, loading: false }));
  }
}

// Sentinel: the fresh window shares no sha with the loaded rows (caller decides
// whether to escalate or replace).
const NO_OVERLAP = Symbol("commit-log-no-overlap");

// Merge a fresh top window into the loaded rows for a refresh: prepend the commits
// above our current top sha (reusing existing objects for the overlapping tail so
// the sha-keyed each does no DOM work, and growing the list). Returns the sentinel
// when the window doesn't reach our top sha so the caller can escalate or replace.
function mergeRefresh(
  state: CommitLogState,
  fresh: CommitInfo[],
  worktreeId: string,
): CommitLogState | typeof NO_OVERLAP {
  const topSha = state.commits[0]?.id;
  const overlap = topSha ? fresh.findIndex((c) => c.id === topSha) : -1;
  if (overlap < 0) return NO_OVERLAP;
  const prepended = fresh.slice(0, overlap);
  // overlap === 0 means our top commit is still HEAD's first row — nothing new to
  // prepend; leave the rows (and scroll) exactly as they are.
  if (prepended.length === 0) {
    return { ...state, loading: false, prependedCount: 0 };
  }
  return {
    ...state,
    worktreeId,
    commits: [...prepended, ...state.commits],
    hasMore: state.hasMore,
    loading: false,
    prependedCount: prepended.length,
    tick: state.tick + 1,
  };
}

// Load (reset to) the first page of the selected worktree's commit log. Used on
// worktree switch / first open, where clearing the rows is correct.
export function loadCommitLog(): Promise<void> {
  return loadCommitLogPage("reset");
}

// Re-fetch the selected worktree's log in place after its HEAD moved, PREPENDING
// only the genuinely-new top commits above the rows already loaded. Existing rows
// keep their object identity (the sha-keyed each does no DOM work for them) and
// the full paginated depth is preserved; the History list compensates scrollTop
// for the prepended height so the viewport stays anchored. This keeps a late-row
// click stable under a churning HEAD (a row never slides out from under the
// pointer between mousedown and mouseup) — the reported "click does nothing /
// snaps to start" failure.
export function refreshCommitLog(): Promise<void> {
  return loadCommitLogPage("refresh");
}

// Append the next page of commits (lazy "load more" at the scroll bottom). A
// no-op while a page is already in flight or when there are no more commits.
export function loadMoreCommits(): Promise<void> {
  if (get(commitLog).loading) return Promise.resolve();
  return loadCommitLogPage("append");
}

// ---- Commit diff cache (Commit Tab) ---------------------------------------
//
// Commit objects are immutable, so a commit's diff never needs invalidation
// (unlike the working-tree diff cache variants). A Commit Tab opened twice
// fetches once; every later open serves the cache. Entries are dropped on
// `disposeDaemon`/`sweepWorktreeCaches`, OR evicted once the cache exceeds
// `COMMIT_DIFF_CACHE_CAP`. Each entry pins a commit's full multi-file
// CommitDiffData, so the cap is deliberately small (mirroring the Rust side's
// LINE_TOTALS_CACHE_CAP): a miss just re-walks via the daemon, the accepted
// fallback. Eviction is FIFO over Map insertion order (oldest entry dropped);
// a read re-inserts its entry so it counts as freshly used (LRU-ish).
export type CommitDiffData = { meta: CommitMeta; files: CommitFileDiff[] };
const COMMIT_DIFF_CACHE_CAP = 64;
const commitDiffCache = new Map<string, CommitDiffData>();
// Coalesce concurrent fetches of the same commit (e.g. a re-click while the first
// request is still in flight) onto one daemon round-trip.
const commitDiffInFlight = new Map<string, Promise<CommitDiffData | null>>();
const commitDiffCacheKey = (worktreeId: Id, sha: string) => `${worktreeId}\0${sha}`;

// Fetch one commit's metadata + per-file diff, serving the immutable per-sha
// cache after the first fetch. Returns `null` on error (the caller renders an
// error/empty state). Concurrent calls for the same commit share one request.
export async function fetchCommitDiff(
  worktreeId: Id,
  sha: string,
): Promise<CommitDiffData | null> {
  const cacheKey = commitDiffCacheKey(worktreeId, sha);
  const cached = commitDiffCache.get(cacheKey);
  if (cached !== undefined) {
    // Re-insert so a served entry moves to the back (newest) of the eviction
    // order — keeps actively-viewed commits from being dropped under the cap.
    commitDiffCache.delete(cacheKey);
    commitDiffCache.set(cacheKey, cached);
    return cached;
  }
  const inFlight = commitDiffInFlight.get(cacheKey);
  if (inFlight) return inFlight;

  const promise = (async (): Promise<CommitDiffData | null> => {
    try {
      const request: CommitDiffRequest = {
        type: "commit-diff",
        worktree_id: worktreeId,
        commit_id: sha,
      };
      const response = await daemonRequest<
        Response & { meta: CommitMeta; files: CommitFileDiff[] }
      >(request);
      const data: CommitDiffData = { meta: response.meta, files: response.files };
      // Evict the oldest entry (front of Map insertion order) when at cap before
      // inserting the newest, bounding the memory each large diff would pin.
      if (commitDiffCache.size >= COMMIT_DIFF_CACHE_CAP && !commitDiffCache.has(cacheKey)) {
        const oldest = commitDiffCache.keys().next().value;
        if (oldest !== undefined) commitDiffCache.delete(oldest);
      }
      commitDiffCache.set(cacheKey, data);
      return data;
    } catch (err) {
      error.set(toMessage(err));
      return null;
    } finally {
      commitDiffInFlight.delete(cacheKey);
    }
  })();
  commitDiffInFlight.set(cacheKey, promise);
  return promise;
}

// Open (or re-activate) the Commit Tab for `sha`: a peer of file diff tabs and
// the all-changes sentinel in `diffTabs`, keyed by the `\0commit:<sha>` path.
// Clicking a commit that's already open re-activates its tab rather than spawning
// a duplicate, mirroring `viewDiff`/`viewAllChanges`. The diff body is fetched
// (and cached) through `fetchCommitDiff` by the tab component, not here.
export function openCommitTab(sha: string): void {
  const path = commitTabPath(sha);
  diffTabs.update((tabs) =>
    tabs.some((tab) => tab.path === path) ? tabs : [...tabs, { path, text: null }],
  );
  activeDiffPath.set(path);
  diffActive.set(true);
}

async function forEachBounded<T>(
  items: readonly T[],
  concurrency: number,
  worker: (item: T, index: number) => Promise<void>,
): Promise<void> {
  if (items.length === 0) return;
  let nextIndex = 0;
  const workerCount = Math.min(Math.max(1, concurrency), items.length);
  await Promise.all(
    Array.from({ length: workerCount }, async () => {
      for (;;) {
        const index = nextIndex;
        nextIndex += 1;
        if (index >= items.length) return;
        await worker(items[index]!, index);
      }
    }),
  );
}

let statusRequestSeq = 0;
let statusPollTimer: ReturnType<typeof setInterval> | null = null;
let statusPollInFlight = false;
let prPollTimer: ReturnType<typeof setInterval> | null = null;
let prFocusHandler: (() => void) | null = null;

const STATUS_POLL_MS = 1_000;
// PR state changes on GitHub (e.g. draft → ready) are external, so poll for them
// while connected. `gh pr list` is network-priced and PR state rarely flips, and
// any change *we* cause is refreshed immediately by the post-git-op and focus
// paths — so the periodic poll only needs to catch external changes, where a few
// minutes of latency is fine. We also skip it while the window is hidden (the
// focus refresh catches up on return), so a backgrounded app spawns no `gh`.
const PR_POLL_MS = 180_000;

// ---- derived stores -------------------------------------------------------

export const selectedProject = derived(
  [projects, selectedProjectId],
  ([$projects, $id]) => $projects.find((p) => p.id === $id) ?? null,
);

// Projects grouped by their owning daemon scope, for the multi-daemon tree. The
// project order WITHIN a scope is preserved from `projects` (daemon order). The
// tree iterates `daemonScopesOrdered` and reads each scope's bucket here, so a
// scope with no projects still renders its (currently only Local) header. Keyed
// by scope id; a project with no explicit tag falls into Local (the only scope
// this slice ingests).
export const projectsByScope = derived([projects, projectScopes], ([$projects, $scopes]) => {
  const map: Record<DaemonScopeId, Project[]> = {};
  for (const project of $projects) {
    const scopeId = $scopes[project.id] ?? LOCAL_SCOPE_ID;
    (map[scopeId] ??= []).push(project);
  }
  return map;
});

export const projectWorktrees = derived(
  [worktrees, selectedProjectId],
  ([$worktrees, $id]) => $worktrees.filter((w) => w.project_id === $id),
);

// The terminal/diff "parent": a git-backed project drives off its selected
// worktree; a plain project is itself the parent (terminals only, no worktrees).
export const selectedParent = derived(
  [selectedProject, selectedWorktreeId],
  ([$project, $worktreeId]): SessionParent | null => {
    if ($worktreeId) return { kind: "worktree", id: $worktreeId };
    if ($project?.kind === "plain") return { kind: "project", id: $project.id };
    return null;
  },
);

export const gitWorktreeId = derived(selectedParent, ($parent) =>
  $parent?.kind === "worktree" ? $parent.id : null,
);

export const currentDirty = derived(
  [gitWorktreeId, dirtyWorktrees],
  ([$id, $dirty]) => ($id ? $dirty[$id] : undefined),
);

// The base branch for PR-base defaults and "from {base}" labels. The DAEMON owns
// the convention now (GitStatus.base_branch, shared with git_log's ahead-of-base
// markers) — so when a git status is loaded for the selected worktree, the
// daemon-provided value WINS: it is the single definition, and only it correctly
// handles the remote-less-worktree fallback that the client can't see.
//
// Two cases keep the worktree-derived value as a fallback rather than regressing:
//  - Rolling upgrade: an OLD daemon omits the field entirely (`undefined`). We
//    can't trust its absence as "no base", so we recompute client-side.
//  - No status yet: `gitStatus` is null before a worktree is selected/loaded, and
//    consumers like CreateWorktreeDialog run with no selection at all. The
//    worktree-derived main branch is available as soon as worktrees load, so we
//    keep it for that window (matching the prior behavior, never null when a main
//    worktree exists).
// A new daemon that explicitly resolves to `null` (the main worktree relative to
// itself — no cross-branch base) is authoritative ONLY when its status is the one
// for the selected worktree; otherwise the stale/absent status doesn't speak for
// the selection, so we fall back.
export const defaultBase = derived(
  [projectWorktrees, gitStatus, gitWorktreeId],
  ([$worktrees, $status, $worktreeId]) => {
    const worktreeDerived = $worktrees.find((w) => w.is_main)?.branch ?? null;
    // Only let the daemon's base speak when the loaded status is for the
    // currently-selected worktree (per-worktree, arrives later than worktrees).
    if ($status && $status.worktree_id === $worktreeId && "base_branch" in $status) {
      // New daemon: `string` is the resolved base, explicit `null` means "no
      // cross-branch base" for this worktree — both authoritative.
      return $status.base_branch ?? null;
    }
    // Old daemon (field absent) or no/foreign status loaded yet: recompute.
    return worktreeDerived;
  },
);

export const visibleSessions = derived(
  [sessions, selectedParent],
  ([$sessions, $parent]) => $sessions.filter((s) => sessionBelongsTo(s, $parent)),
);

export const activeSession = derived(
  [sessions, activeSessionId],
  ([$sessions, $id]) => $sessions.find((s) => s.id === $id) ?? null,
);

// The visual tab order, exactly as SessionTabs.svelte renders the strip: every
// visible session first (in `visibleSessions` order), then every open diff tab
// (in `diffTabs` order, the all-changes sentinel included as a normal entry).
// A descriptor's `id` is the session id for sessions and the (worktree-relative
// or sentinel) path for diffs — the same key each command needs to re-activate
// or close that tab. Keyboard tab commands (Cmd+1–9, next/prev) index into this
// so their N matches what the user sees, and `activeTabIndex` reports where the
// current center view sits in it.
export type TabDescriptor = { kind: "session"; id: Id } | { kind: "diff"; id: string };
export const orderedTabs = derived(
  [visibleSessions, diffTabs],
  ([$sessions, $diffTabs]): TabDescriptor[] => [
    ...$sessions.map((s): TabDescriptor => ({ kind: "session", id: s.id })),
    ...$diffTabs.map((t): TabDescriptor => ({ kind: "diff", id: t.path })),
  ],
);

// Index of the currently-active tab in `orderedTabs`, or -1 when nothing maps
// (no sessions and no diffs). Mirrors SessionTabs' active rule: a diff tab is
// active iff `diffActive` (then it's `activeDiffPath`); otherwise the active
// session is `activeSessionId`. Pure read off the stores so next/prev can pivot
// from the current selection.
export function activeTabIndex(): number {
  const tabs = get(orderedTabs);
  if (get(diffActive)) {
    const path = get(activeDiffPath);
    return tabs.findIndex((t) => t.kind === "diff" && t.id === path);
  }
  const id = get(activeSessionId);
  return tabs.findIndex((t) => t.kind === "session" && t.id === id);
}

// Activate the tab at `index` in `orderedTabs` (the visual order). Out-of-range
// is a no-op (Cmd+N with fewer than N tabs). A session tab clears the diff view
// and selects the session; a diff tab activates that path — the same state moves
// SessionTabs.select()/selectDiff() make. Terminal DOM focus is the caller's job
// (the layout owns the focuser registry); this only moves daemon state.
export function activateTabIndex(index: number): void {
  const tabs = get(orderedTabs);
  const tab = tabs[index];
  if (!tab) return;
  if (tab.kind === "session") {
    diffActive.set(false);
    activeSessionId.set(tab.id);
  } else {
    activeDiffPath.set(tab.id);
    diffActive.set(true);
  }
}

// Per-worktree rollup of the DISPLAY state (act states always; `running` only
// while its output gate is open; `waiting` is unlabeled and never rolls up).
export const agentStateByWorktree = derived(
  [worktrees, sessions, displaySessionStates],
  ([$worktrees, $sessions, $display]) => {
    const map: Record<Id, AgentState> = {};
    for (const worktree of $worktrees) {
      const agg = aggregateAgentState(
        $sessions
          .filter((s) => s.parent.kind === "worktree" && s.parent.id === worktree.id)
          .map((s) => $display[s.id]),
      );
      if (!agg) continue;
      map[worktree.id] = agg;
    }
    return map;
  },
);

// Per-project act-state rollup: the highest-priority act state across the
// project's sessions plus the count of sessions in an act state (ADR 0011
// amendment / CONTEXT.md). One pill, mixed act states collapse to the highest
// priority. `null` = nothing in the project needs action. The rollup counts the
// raw hook act states directly (act states are never output-gated), so a
// collapsed project never hides a session demanding attention.
export const agentActRollupByProject = derived(
  [projects, worktrees, sessions, agentStates],
  ([$projects, $worktrees, $sessions, $agentStates]) => {
    const map: Record<Id, ActRollup> = {};
    for (const project of $projects) {
      const projectWorktreeIds = new Set(
        $worktrees.filter((w) => w.project_id === project.id).map((w) => w.id),
      );
      const states = $sessions
        .filter(
          (s) =>
            (s.parent.kind === "worktree" && projectWorktreeIds.has(s.parent.id)) ||
            (s.parent.kind === "project" && s.parent.id === project.id),
        )
        .map((s) => $agentStates[s.id]);
      const rollup = aggregateActRollup(states);
      if (rollup) map[project.id] = rollup;
    }
    return map;
  },
);

// ---- request helper -------------------------------------------------------

function isError(response: Response): response is Response & {
  error: { message: string };
} {
  return response.type === "error";
}

export async function daemonRequest<T extends Response>(
  request: Request,
): Promise<T> {
  const response = await invoke<Response>("hitch_request", { request });
  if (isError(response)) {
    throw new Error(response.error.message);
  }
  return response as T;
}

function toMessage(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

// ---- Job dispatch (ADR 0008) ----------------------------------------------
//
// `runJob` wraps a job-capable request in `StartJob`, registers a pending
// resolver keyed by the returned job id, and resolves/rejects it when the
// matching `JobCompleted` event arrives (handled in the hitch-event listener).
// Callers (push/pull/draft fns) keep an ordinary Promise API.
type JobPending = {
  resolve: (response: Response) => void;
  reject: (error: Error) => void;
};
const jobPending = new Map<Id, JobPending>();
const EARLY_COMPLETION_TTL_MS = 30_000;
const EARLY_COMPLETION_MAX = 64;
type EarlyCompletion = { response: Response; receivedAt: number };
const earlyCompletions = new Map<Id, EarlyCompletion>();
const locallyStartedJobs = new Set<Id>();
let startingJobRequests = 0;
const cancellableJobKinds = new Set([
  "clone",
  "create-worktree",
  "push",
  "fetch",
  "pull",
  "create-pr",
  "commit-and-push",
  "draft-models",
  "commit-draft",
  "pr-draft",
]);

export function isJobCancellable(job: Job | null | undefined): boolean {
  return Boolean(job?.kind && cancellableJobKinds.has(job.kind));
}

function jobWorktreeId(request: JobRequest): Id | null {
  return "worktree_id" in request ? request.worktree_id : null;
}

export const cancellableJobForSelectedWorktree = derived(
  [jobs, gitWorktreeId],
  ([$jobs, $worktreeId]) => {
    if (!$worktreeId) return null;
    return (
      Object.values($jobs).find(
        (job) =>
          job.worktreeId === $worktreeId &&
          (job.status === "running" || job.status === "queued") &&
          isJobCancellable(job),
      ) ?? null
    );
  },
);

function pruneEarlyCompletions(now = Date.now()): void {
  for (const [jobId, completion] of earlyCompletions) {
    if (now - completion.receivedAt > EARLY_COMPLETION_TTL_MS) {
      earlyCompletions.delete(jobId);
    }
  }
  while (earlyCompletions.size > EARLY_COMPLETION_MAX) {
    const oldest = earlyCompletions.keys().next().value;
    if (oldest === undefined) break;
    earlyCompletions.delete(oldest);
  }
}

function rememberEarlyCompletion(jobId: Id, response: Response): void {
  earlyCompletions.set(jobId, { response, receivedAt: Date.now() });
  pruneEarlyCompletions();
}

function takeEarlyCompletion(jobId: Id): Response | null {
  pruneEarlyCompletions();
  const early = earlyCompletions.get(jobId);
  if (!early) return null;
  earlyCompletions.delete(jobId);
  return early.response;
}

export async function runJob<T extends Response>(
  request: JobRequest,
  kind: string | null = null,
): Promise<T> {
  startingJobRequests += 1;
  let started: Response & { job_id: Id };
  try {
    const startRequest: StartJobRequest = { type: "start-job", request };
    started = await daemonRequest<Response & { job_id: Id }>(startRequest);
  } finally {
    startingJobRequests = Math.max(0, startingJobRequests - 1);
  }
  const jobId = started.job_id;
  locallyStartedJobs.add(jobId);
  jobs.update((current) => ({
    ...current,
    [jobId]: { id: jobId, status: "running", message: null, kind, worktreeId: jobWorktreeId(request) },
  }));
  return new Promise<T>((resolve, reject) => {
    const early = takeEarlyCompletion(jobId);
    if (early) {
      locallyStartedJobs.delete(jobId);
      jobs.update((current) => {
        const next = { ...current };
        delete next[jobId];
        return next;
      });
      if (isError(early)) {
        reject(new Error(early.error.message));
      } else {
        resolve(early as T);
      }
      return;
    }
    jobPending.set(jobId, {
      resolve: (response) => resolve(response as T),
      reject,
    });
  });
}

// Update the live Jobs store from a `JobProgress` event. Exported as the seam
// the event listener and the unit tests both drive.
export function applyJobProgress(
  jobId: Id,
  status: JobStatus,
  message: string | null,
  kind: string | null = null,
): void {
  jobs.update((current) => ({
    ...current,
    [jobId]: {
      id: jobId,
      status,
      message,
      kind: kind ?? current[jobId]?.kind ?? null,
      worktreeId: current[jobId]?.worktreeId ?? null,
    },
  }));
}

// Resolve a Job from its `JobCompleted` event: the wrapped response rides inside.
export function completeJob(jobId: Id, response: Response): void {
  // Resolve a composite chain's per-worktree DISPLAY state FIRST (before the job
  // is dropped from the store below, so the worktree lookup still works): clear
  // on success, park the oxide failure on a mid-chain failure. The worktree id
  // rides the live Jobs store entry or the chain display whose jobId matches.
  if (
    response.type === "composite-job-failed" ||
    response.type === "commit-and-pushed" ||
    response.type === "pull-request-created"
  ) {
    const worktreeId =
      get(jobs)[jobId]?.worktreeId ??
      Object.entries(get(compositeChains)).find(([, c]) => c.jobId === jobId)?.[0] ??
      null;
    if (worktreeId) applyCompositeCompletion(worktreeId, response);
  } else if (isError(response)) {
    // A composite Job can ALSO terminate as a plain `Response::Error` rather than
    // a `composite-job-failed`: cancellation, the worktree vanishing at chain
    // start, a poisoned lock, or a worker panic. None of those reach
    // applyCompositeCompletion above, so without this the optimistic in-flight
    // display would freeze (spinner stuck, autoRunning stuck true). Park it as a
    // failure so the button shows the oxide ✗ + retry instead. The worktree id
    // rides the live Jobs store entry or the chain display whose jobId matches
    // (same resolution as the success path). Park ONLY if a chain entry still
    // exists for it — a user-driven cancel optimistically clears the chain before
    // its "job cancelled" error arrives, and we must not resurrect it.
    const worktreeId =
      get(jobs)[jobId]?.worktreeId ??
      Object.entries(get(compositeChains)).find(([, c]) => c.jobId === jobId)?.[0] ??
      null;
    if (worktreeId && get(compositeChains)[worktreeId]) {
      applyCompositeErrorCompletion(worktreeId, response.error.message);
    }
  }
  const pending = jobPending.get(jobId);
  const local = locallyStartedJobs.has(jobId);
  jobPending.delete(jobId);
  jobs.update((current) => {
    if (!(jobId in current)) return current;
    const next = { ...current };
    delete next[jobId];
    return next;
  });
  if (!pending) {
    if (local || startingJobRequests > 0) {
      rememberEarlyCompletion(jobId, response);
    }
    return;
  }
  locallyStartedJobs.delete(jobId);
  if (isError(response)) {
    pending.reject(new Error(response.error.message));
  } else {
    pending.resolve(response);
  }
}

// Reject every in-flight Job when the daemon link drops. Jobs are ephemeral and
// do NOT survive a daemon restart (CONTEXT.md): a Job that was running when the
// daemon stopped is reported failed so the user can re-trigger.
function failAllJobs(reason: string): void {
  const message = `daemon restarted: ${reason}`;
  for (const [, pending] of jobPending) {
    pending.reject(new Error(message));
  }
  jobPending.clear();
  earlyCompletions.clear();
  locallyStartedJobs.clear();
  jobs.set({});
  // Composite chains are ephemeral too: a daemon drop strands every in-flight
  // chain display. (A re-attach re-queries `active-jobs` to restore the truth.)
  compositeChains.set({});
}

// Ask the daemon to cancel a running Job (signals its worker to kill any child).
export async function cancelJob(jobId: Id): Promise<void> {
  try {
    await daemonRequest({ type: "cancel-job", job_id: jobId });
  } catch (err) {
    error.set(toMessage(err));
  }
}

// ---- composite chains (ADR 0013 amendment 2026-06-07) ---------------------
//
// The two autonomous chains — auto commit & push and PR create — run as ONE
// daemon-owned Job each. Their per-step progress is broadcast as
// `composite-job-progress` events and their result rides the normal
// `job-completed` envelope. The chain is daemon-owned, so its *display* state
// must NOT live in a component (which unmounts on navigation): it lives here, in
// a per-worktree store, fed by the progress events AND seeded by the
// `active-jobs` query a (re)attaching GUI runs. RightRail's auto-mode header
// morph and the Composer's PR progress both read this one store, so leaving and
// returning mid-chain restores the exact in-flight step.

// The live state of one worktree's in-flight chain (at most one per worktree —
// the daemon serializes mutating git ops per worktree). `step` is the current
// rung; `failed` carries the terminal failure for the oxide retry state.
export type CompositeChainState = {
  jobId: Id | null;
  kind: CompositeJobKind;
  step: CompositeStep;
  // A terminal failure: the chain stopped on `failed.step` with `failed.reason`.
  // The button shows the oxide `✗ <step> failed — retry` state until retried or
  // dismissed; `step` stays the failed rung for the label.
  failed: { step: CompositeStep; reason: string } | null;
};
// Per worktree id. Absent = no chain in flight (and no unacknowledged failure).
export const compositeChains = writable<Record<Id, CompositeChainState>>({});

// The chain for the currently-selected worktree (what RightRail / Composer read).
export const compositeChainForSelectedWorktree = derived(
  [compositeChains, gitWorktreeId],
  ([$chains, $worktreeId]) => ($worktreeId ? ($chains[$worktreeId] ?? null) : null),
);

function setCompositeChain(worktreeId: Id, state: CompositeChainState): void {
  compositeChains.update((current) => ({ ...current, [worktreeId]: state }));
}

// Clear a worktree's chain display (success ack, dismissed failure, or a fresh
// retry replacing the old failed state).
export function clearCompositeChain(worktreeId: Id): void {
  compositeChains.update((current) => omitKey(current, worktreeId));
}

// Apply a `composite-job-progress` event: a step `started` becomes the current
// rung; we ignore `finished` for display (the next step's `started` advances the
// label, and the terminal step's result clears the chain on completion). A live
// progress event also clears any stale failure for that worktree.
export function applyCompositeJobProgress(
  jobId: Id,
  worktreeId: Id,
  kind: CompositeJobKind,
  step: CompositeStep,
  phase: StepPhase,
): void {
  if (phase !== "started") return;
  setCompositeChain(worktreeId, { jobId, kind, step, failed: null });
}

// Resolve a chain on its `job-completed`. Success clears the chain display
// (RightRail flashes its calm "committed"/PR state via the completion handlers,
// then the chip/diffstat refresh takes over); a `composite-job-failed` parks the
// oxide retry state on the worktree.
function applyCompositeCompletion(worktreeId: Id, response: Response): void {
  if (response.type === "composite-job-failed") {
    const failedStep = response.failed_step as CompositeStep;
    const reason = (response.reason as string | null) ?? "Chain failed.";
    const kind = response.kind as CompositeJobKind;
    setCompositeChain(worktreeId, {
      jobId: null,
      kind,
      step: failedStep,
      failed: { step: failedStep, reason },
    });
    return;
  }
  // Success (commit-and-pushed / pull-request-created) — drop the in-flight
  // display; the calling start fn handles the auto-mode toast / status refresh.
  clearCompositeChain(worktreeId);
  // PR chain finale: OPEN the created draft PR in the browser. The daemon never
  // opens it (ADR 0013 amendment) — the GUI does, here, only because this handler
  // only runs while attached. Centralized here (not in the confirm path) so a
  // chain RESTORED after navigating away also opens on completion, and a single
  // open guard prevents a double-open. If no GUI is attached when it completes,
  // nothing opens and the rail's PR chip reflects it on next attach.
  if (response.type === "pull-request-created" && typeof response.url === "string") {
    openCreatedPrUrl(worktreeId, response.url);
  }
}

// Park a chain that terminated as a plain `Response::Error` (cancellation,
// worktree removed at chain start, poisoned lock, worker panic) — none of which
// carry a `failed_step`/`kind`, so we keep the chain's current rung (the
// optimistic or last-progress step) and reuse its kind for the oxide retry
// label. Mirrors the `composite-job-failed` parking shape so RightRail/Composer
// render ✗ + retry identically.
function applyCompositeErrorCompletion(worktreeId: Id, reason: string): void {
  const held = get(compositeChains)[worktreeId];
  if (!held) return;
  setCompositeChain(worktreeId, {
    jobId: null,
    kind: held.kind,
    step: held.step,
    failed: { step: held.step, reason: reason || "Chain failed." },
  });
}

// PR urls already opened, so a duplicate completion (or a confirm path that also
// tried) can't open the browser twice for the same chain.
const openedPrUrls = new Set<string>();
function openCreatedPrUrl(worktreeId: Id, url: string): void {
  if (openedPrUrls.has(url)) return;
  openedPrUrls.add(url);
  void (async () => {
    try {
      const { openUrl } = await import("@tauri-apps/plugin-opener");
      await openUrl(url);
    } catch {
      // Best-effort: the rail's PR chip still links to the created PR.
    }
  })();
}

// Start the auto commit-and-push chain for a worktree. Resolves with the
// `CommitAndPushResult` (subject · short sha · pushed · files) for the toast.
// Routes the COMMIT draft instructions per slice 5. A failure rejects with the
// chain's failed-step reason (the oxide retry state is already parked by the
// completion handler).
export async function startCommitAndPush(
  worktreeId: Id | null = get(gitWorktreeId),
): Promise<CommitAndPushResult> {
  if (!worktreeId) throw new Error("Select a git worktree first.");
  // Optimistically show the first step so the button morphs immediately, even
  // before the daemon's first progress event lands.
  setCompositeChain(worktreeId, {
    jobId: null,
    kind: "commit-and-push",
    step: "staging",
    failed: null,
  });
  const response = await runJob<Response & { result?: CommitAndPushResult }>(
    {
      type: "commit-and-push",
      worktree_id: worktreeId,
      settings: draftGenerationSettings({ commit: get(draftCommitInstructions) }),
    },
    "commit-and-push",
  );
  // A mid-chain failure rides the normal `job-completed` envelope as a
  // `composite-job-failed` response (not an `error`), so runJob RESOLVES it. The
  // oxide retry state is already parked by the completion handler; surface the
  // reason to the caller so the toast goes error.
  throwIfCompositeFailed(response);
  // Refresh status + PR chip so the rail reflects the pushed commit.
  void loadGitStatus(worktreeId).catch(() => {});
  void loadPrStatus(worktreeId);
  if (!response.result) throw new Error("Commit-and-push completed without a result.");
  return response.result;
}

// Throw the chain's failed-step reason when a composite Job resolved as a
// `composite-job-failed` (runJob resolves it — only `type: "error"` rejects).
function throwIfCompositeFailed(response: Response): void {
  if (response.type === "composite-job-failed") {
    const step = response.failed_step as string;
    const reason = (response.reason as string | null) ?? "Chain failed.";
    throw new Error(`${step} failed — ${reason}`);
  }
}

// Start the autonomous PR chain for a worktree (push → draft → create GitHub
// DRAFT PR). Resolves with the created PR url so the GUI can open the browser —
// only the daemon never opens it. Routes the PR draft instructions per slice 5.
export async function startCreatePr(
  base: string | null,
  worktreeId: Id | null = get(gitWorktreeId),
): Promise<string> {
  if (!worktreeId) throw new Error("Select a git worktree first.");
  setCompositeChain(worktreeId, {
    jobId: null,
    kind: "create-pr",
    step: "pushing",
    failed: null,
  });
  const response = await runJob<Response & { url?: string }>(
    {
      type: "create-pr",
      worktree_id: worktreeId,
      base: base?.trim() || null,
      settings: draftGenerationSettings({ pr: get(draftPrInstructions) }),
    },
    "create-pr",
  );
  throwIfCompositeFailed(response);
  void loadGitStatus(worktreeId).catch(() => {});
  void loadPrStatus(worktreeId);
  if (!response.url) throw new Error("Create-PR completed without a url.");
  return response.url;
}

// Query a worktree's in-flight composite chains and seed the display store, so a
// (re)attaching GUI restores the exact button/Composer step. Returns the
// restored entry (or null) for callers that want to also keep following events.
// Only composite chains are reported; an empty reply clears any stale display
// that is not a parked failure (a failure is local terminal state, not in-flight,
// so it must survive a refresh).
export async function refreshActiveJobs(
  worktreeId: Id | null = get(gitWorktreeId),
): Promise<CompositeChainState | null> {
  if (!worktreeId) return null;
  let response: Response & { jobs: ActiveJobInfo[] };
  try {
    response = await daemonRequest<Response & { jobs: ActiveJobInfo[] }>({
      type: "active-jobs",
      worktree_id: worktreeId,
    });
  } catch {
    return null;
  }
  const active = response.jobs.find((j) => j.worktree_id === worktreeId) ?? response.jobs[0];
  if (active) {
    const state: CompositeChainState = {
      jobId: active.job_id,
      kind: active.kind,
      step: active.step,
      failed: null,
    };
    setCompositeChain(worktreeId, state);
    return state;
  }
  // No chain running. Drop any in-flight display we hold UNLESS it is a parked
  // failure (terminal local state the daemon doesn't track).
  const held = get(compositeChains)[worktreeId];
  if (held && !held.failed) clearCompositeChain(worktreeId);
  return null;
}

// Cancel a worktree's in-flight composite chain (the × in the morphed header).
// Resolves the Job id from the chain display, falling back to the live Jobs
// store for the window before the first progress event sets the chain's jobId.
export async function cancelCompositeChain(
  worktreeId: Id | null = get(gitWorktreeId),
): Promise<void> {
  if (!worktreeId) return;
  const chain = get(compositeChains)[worktreeId];
  const jobId =
    chain?.jobId ??
    Object.values(get(jobs)).find(
      (job) =>
        job.worktreeId === worktreeId &&
        (job.kind === "commit-and-push" || job.kind === "create-pr"),
    )?.id ??
    null;
  if (jobId) await cancelJob(jobId);
  // Clear the optimistic display NOW — this is the clear that actually settles
  // the button. The daemon's cancellation arrives as a plain error-typed
  // `job-completed`, which completeJob's composite-error path would otherwise
  // PARK as an oxide failure; clearing here first means no chain entry survives
  // for that path to park (the park is guarded on a still-present entry), so the
  // button settles instead of flipping to ✗. This also covers the cancel racing
  // the first progress event.
  clearCompositeChain(worktreeId);
}

function upsert<T extends { id: Id }>(items: T[], item: T): T[] {
  return items.some((existing) => existing.id === item.id)
    ? items.map((existing) => (existing.id === item.id ? item : existing))
    : [...items, item];
}

function omitKey<T>(record: Record<Id, T>, id: Id): Record<Id, T> {
  if (!(id in record)) return record;
  const next = { ...record };
  delete next[id];
  return next;
}

// Evict every per-worktree cache/bookkeeping entry belonging to a worktree.
// The diff caches key by `${worktreeId}\0…` (working-tree diff variants use
// `\0<path>\0<options>`, commit diffs use `\0<sha>`), so a single prefix sweep
// covers them; the PR-freshness maps key by bare worktree id. Called when a
// worktree disappears (removed directly, or as part of a removed project) — the
// entries (especially commit diffs, which hold full multi-file CommitDiffData and
// otherwise live until disposeDaemon) would leak forever otherwise. `Map.delete`
// is idempotent, so callers may also delete these directly without double-free.
function sweepWorktreeCaches(worktreeId: Id): void {
  const prefix = `${worktreeId}\0`;
  for (const map of [
    diffCache,
    diffRequestSeq,
    diffCacheWriteSeq,
    commitDiffCache,
    commitDiffInFlight,
  ]) {
    for (const key of map.keys()) {
      if (key.startsWith(prefix)) map.delete(key);
    }
  }
  prByWorktreeApplied.delete(worktreeId);
  prByWorktreeStarted.delete(worktreeId);
}

function removeWorktreeLocal(worktreeId: Id): void {
  const removedSessionIds = get(sessions)
    .filter((session) => session.parent.kind === "worktree" && session.parent.id === worktreeId)
    .map((session) => session.id);
  for (const sessionId of removedSessionIds) {
    closeSessionOutput(sessionId);
  }

  sessions.update((items) =>
    items.filter((session) => session.parent.kind !== "worktree" || session.parent.id !== worktreeId),
  );
  worktrees.update((items) => items.filter((worktree) => worktree.id !== worktreeId));
  dirtyWorktrees.update((current) => omitKey(current, worktreeId));
  worktreeLineStats.update((current) => omitKey(current, worktreeId));
  compositeChains.update((current) => omitKey(current, worktreeId));
  prByWorktree.update((current) => omitKey(current, worktreeId));
  // sweepWorktreeCaches also drops prByWorktreeApplied/prByWorktreeStarted.
  sweepWorktreeCaches(worktreeId);

  if (get(gitStatus)?.worktree_id === worktreeId) gitStatus.set(null);
  if (get(gitWorktreeId) === worktreeId) {
    selectedWorktreeId.set(null);
    prInfo.set(null);
    prUrl.set(null);
    closeDiff();
  }
}

// ---- snapshot / refresh ---------------------------------------------------

export async function refreshAll(
  options: { restoreLiveSessionSelection?: boolean } = {},
): Promise<void> {
  const projectResponse = await daemonRequest<Response & { projects: Project[] }>({
    type: "list-projects",
  });
  projects.set(projectResponse.projects);
  // Every project from `list-projects` over the local socket is owned by the
  // Local scope (ADR 0014). A full snapshot replaces the scope map wholesale so
  // a project removed out of band loses its tag. Issue #28 will instead tag a
  // remote daemon's projects with its SSH Host scope at its own ingestion.
  setProjectScopes(
    projectResponse.projects.map((project) => project.id),
    LOCAL_SCOPE_ID,
  );

  const worktreeLists = await Promise.all(
    projectResponse.projects
      .filter((project) => project.kind === "git-backed")
      .map(async (project) => {
        const response = await daemonRequest<Response & { worktrees: Worktree[] }>({
          type: "list-worktrees",
          project_id: project.id,
        });
        return response.worktrees;
      }),
  );
  const allWorktrees = worktreeLists.flat();
  worktrees.set(allWorktrees);

  // Populate every worktree's PR chip in the background — one batched lookup per
  // project, throttled internally. Not awaited: a slow `gh` must not hold up the
  // rest of the snapshot (sessions, dirty state).
  for (const project of projectResponse.projects) {
    if (project.kind === "git-backed") {
      void loadProjectPrStatuses(project.id);
    }
  }

  const sessionResponse = await daemonRequest<Response & { sessions: Session[] }>({
    type: "list-sessions",
    parent: null,
  });
  sessions.set(sessionResponse.sessions);
  if (options.restoreLiveSessionSelection) {
    restoreLiveSessionSelection(sessionResponse.sessions, allWorktrees);
  }
  reconcileSessionOutputs(sessionResponse.sessions);

  // Seed dirty indicators and line stats so the tree is useful before the Changes panel opens.
  const statusEntries = await Promise.all(
    allWorktrees.map(async (worktree) => {
      try {
        const response = await daemonRequest<Response & { status: GitStatus }>({
          type: "git-status",
          worktree_id: worktree.id,
        });
        return [worktree.id, response.status] as const;
      } catch {
        return [worktree.id, null] as const;
      }
    }),
  );
  dirtyWorktrees.set(
    Object.fromEntries(statusEntries.map(([id, status]) => [id, status?.dirty ?? false])),
  );
  worktreeLineStats.set(
    Object.fromEntries(
      statusEntries.map(([id, status]) => [
        id,
        { additions: status?.additions ?? 0, deletions: status?.deletions ?? 0 },
      ]),
    ),
  );
}

function restoreLiveSessionSelection(liveSessions: Session[], allWorktrees: Worktree[]): void {
  // The user already has a project/worktree selected — keep it. Whether or not
  // that selection currently has a live session, stay put rather than yanking
  // the user to an unrelated project/worktree. Only a fresh window with no
  // selection auto-jumps to the first live session below.
  if (get(selectedParent)) {
    return;
  }

  for (const session of liveSessions) {
    if (session.parent.kind === "project") {
      if (!get(projects).some((project) => project.id === session.parent.id)) continue;
      selectedProjectId.set(session.parent.id);
      selectedWorktreeId.set(null);
      activeSessionId.set(session.id);
      return;
    }

    const worktree = allWorktrees.find((item) => item.id === session.parent.id);
    if (!worktree) continue;
    selectedProjectId.set(worktree.project_id);
    selectedWorktreeId.set(worktree.id);
    activeSessionId.set(session.id);
    return;
  }
}
async function refreshSnapshotAfterConnect(): Promise<void> {
  try {
    await refreshAll({ restoreLiveSessionSelection: true });
  } catch (err) {
    error.set(toMessage(err));
  }
}


// ---- per-session PTY output (binary channel + bounded byte ring) ----------
//
// PTY bytes stream from the Tauri layer over a per-session binary Channel
// (ADR 0007); they are NEVER stringified in Rust, so xterm's own streaming
// UTF-8 decoder handles glyphs split across read boundaries. Here we hold each
// session's recent bytes in a bounded `ByteRing` (the GUI repaint copy; the
// daemon stays authoritative for scrollback) and fan new tails out to whichever
// Terminal component is currently mounted for that session.

type OutputSubscriber = {
  onReset: () => void;
  onData: (tail: Uint8Array) => void;
};

const rings = new Map<Id, ByteRing>();
const channels = new Map<Id, Channel<ArrayBuffer | Uint8Array>>();
// At most one Terminal is mounted per session at a time (the parent re-keys it),
// so a single subscriber slot per session is sufficient.
const subscribers = new Map<Id, OutputSubscriber>();

// The Channel delivers raw bytes as an ArrayBuffer (tauri 2's `Raw` IPC body);
// normalize whatever form arrives into a Uint8Array for the ring + xterm.
function toUint8Array(msg: ArrayBuffer | Uint8Array): Uint8Array {
  return msg instanceof Uint8Array ? msg : new Uint8Array(msg);
}

function ringFor(sessionId: Id): ByteRing {
  let ring = rings.get(sessionId);
  if (!ring) {
    ring = new ByteRing();
    rings.set(sessionId, ring);
  }
  return ring;
}

// Subscribe a mounted Terminal to a session's output. `onData` is called with
// the current ring contents immediately (catch-up) and again with each new tail;
// `onReset` fires when the ring is reset (a reconnect replay). Returns an
// unsubscribe.
export function subscribeSessionOutput(
  sessionId: Id,
  subscriber: OutputSubscriber,
): () => void {
  subscribers.set(sessionId, subscriber);
  // Catch up to whatever the ring already holds (e.g. output that arrived
  // before this terminal mounted, or a scrollback replay on reconnect).
  const ring = rings.get(sessionId);
  if (ring && ring.length > 0) subscriber.onData(ring.snapshot());
  return () => {
    if (subscribers.get(sessionId) === subscriber) subscribers.delete(sessionId);
  };
}

// Register (or re-register) a session's binary output channel with Tauri and
// reset its ring. Called on session-opened — both for brand-new sessions
// (empty ring, harmless) and on every reconnect, where the daemon replays the
// full scrollback as one SessionOutput; resetting here keeps that replay from
// duplicating the prior bytes.
function openSessionOutput(sessionId: Id): void {
  // Fresh ring so a reconnect replay repopulates from zero (no duplication).
  rings.set(sessionId, new ByteRing());
  subscribers.get(sessionId)?.onReset();

  let replayMode = true;
  const channel = new Channel<ArrayBuffer | Uint8Array>();
  channel.onmessage = (msg) => {
    const bytes = toUint8Array(msg);
    if (bytes.length === 0) return;
    const offsetBefore = ringFor(sessionId).totalSeen;

    // On reconnect, the daemon sends the full replay in one message, which can
    // exceed the ring capacity. Stream it directly to the subscriber while
    // retaining only the tail in the ring for future catch-up.
    if (replayMode) {
      // Send the full replay to the subscriber without truncation.
      subscribers.get(sessionId)?.onData(bytes);
      // Now add it to the ring, accepting that oversized replays will trim to
      // capacity; future live output will be bounded normally.
      ringFor(sessionId).append(bytes);
      replayMode = false;
      return;
    }

    // Normal live output path: bounded append and tail delivery.
    ringFor(sessionId).append(bytes);
    // Hand the subscriber exactly the new tail still retained by the ring.
    subscribers.get(sessionId)?.onData(ringFor(sessionId).bytesSince(offsetBefore));
  };
  channels.set(sessionId, channel);
  void Promise.resolve(invoke("register_session_output", { sessionId, channel })).catch(() => {
    if (channels.get(sessionId) === channel) channels.delete(sessionId);
  });
}

// Tear down a session's output: drop the ring + channel and tell Tauri to stop
// routing (and discard any staged bytes).
function closeSessionOutput(sessionId: Id): void {
  rings.delete(sessionId);
  channels.delete(sessionId);
  // A closing session must not have a trailing resize fire against its dead PTY.
  clearResizeDebounce(sessionId);
  void Promise.resolve(invoke("unregister_session_output", { sessionId })).catch(() => {});
}

function reconcileSessionOutputs(liveSessions: Session[]): void {
  const liveIds = new Set(liveSessions.map((session) => session.id));
  for (const sessionId of Array.from(channels.keys())) {
    if (!liveIds.has(sessionId)) closeSessionOutput(sessionId);
  }
  for (const session of liveSessions) {
    if (!channels.has(session.id)) openSessionOutput(session.id);
  }
}


// Set or clear a session's stored Agent State. `null` is the daemon's clear.
function applyAgentState(sessionId: Id, state: AgentState | null): void {
  if (state) {
    agentStates.update((current) =>
      current[sessionId] === state ? current : { ...current, [sessionId]: state },
    );
  } else {
    agentStates.update((current) => {
      if (!(sessionId in current)) return current;
      const next = { ...current };
      delete next[sessionId];
      return next;
    });
  }
}

// Set or clear a session's announced Agent identity (the Session mark source).
// `null` clears it back to the shell mark (agent exited to `None`).
function applySessionAgent(sessionId: Id, agent: KnownAgent | null): void {
  if (agent) {
    sessionAgents.update((current) =>
      current[sessionId] === agent ? current : { ...current, [sessionId]: agent },
    );
  } else {
    sessionAgents.update((current) => {
      if (!(sessionId in current)) return current;
      const next = { ...current };
      delete next[sessionId];
      return next;
    });
  }
}

// The notification body for a session: `project · branch` of its worktree, so
// an OS notification names *which* run acted without the user opening the app.
// Resolves session → parent → worktree → branch + owning project; degrades to
// the project name alone (plain-project sessions have no worktree), then the
// session name, then null (no body) so the notification still fires unadorned.
function notificationBodyForSession(sessionId: Id): string | null {
  const session = get(sessions).find((s) => s.id === sessionId);
  if (!session) return null;
  if (session.parent.kind === "worktree") {
    const worktree = get(worktrees).find((w) => w.id === session.parent.id);
    if (worktree) {
      const project = get(projects).find((p) => p.id === worktree.project_id);
      return project ? `${project.name} · ${worktree.branch}` : worktree.branch;
    }
  } else {
    const project = get(projects).find((p) => p.id === session.parent.id);
    if (project) return project.name;
  }
  return session.name || null;
}

// Dispatch one agent-state transition to the notification engine, reading the
// PREVIOUS stored state from `agentStates` (the single source of truth) before
// it's overwritten. Both the live event and the SessionOpened replay route
// through here; `replay` tells notifications.ts to set the baseline without
// notifying (replayed state must never ping — ADR 0011 attach replay).
function noteAgentStateTransition(
  sessionId: Id,
  nextState: AgentState | null,
  agent: KnownAgent | null,
  detail: string | null,
  replay: boolean,
): void {
  noteAgentState(sessionId, get(agentStates)[sessionId] ?? null, nextState, detail, {
    replay,
    agent,
    body: notificationBodyForSession(sessionId),
    activeSessionId: get(activeSessionId),
  });
}

export function applyHitchEvent(event: HitchEvent): void {
  if (event.type === "project-updated") {
    const project = event.project as Project;
    projects.update((items) => upsert(items, project));
    // A project pushed over the local socket is Local-scoped (ADR 0014). Keep
    // an existing tag (idempotent) so an upsert never re-homes a project.
    tagProjectScope(project.id, LOCAL_SCOPE_ID);
  }
  if (event.type === "worktree-updated") {
    worktrees.update((items) => upsert(items, event.worktree as Worktree));
  }
  if (event.type === "worktree-removed") {
    removeWorktreeLocal(event.worktree_id as Id);
  }
  if (event.type === "worktree-dirty") {
    const worktreeId = event.worktree_id as Id;
    const dirty = event.dirty as boolean;
    dirtyWorktrees.update((current) =>
      current[worktreeId] === dirty ? current : { ...current, [worktreeId]: dirty },
    );
    requestDiffRefreshOnNextStatus(worktreeId);
    // Refresh the full status so the tree's line stats and, when selected,
    // the Changes panel track filesystem changes live.
    void loadGitStatus(worktreeId).catch(() => {});
  }
  if (event.type === "agent-state") {
    // The daemon broadcasts AgentState both for real state reports and for
    // identity announces (where `state` is null pre-prompt but `agent` is now
    // known). Identity and state are cleared INDEPENDENTLY: `agent: null` is
    // the identity clear (exit-to-`None` reverts the mark to shell), a null
    // state only clears state — it must NOT drop an announced identity, or the
    // Session mark would not render until the first prompt (ADR 0011 amendment).
    const sessionId = (event.session_id as Id | null) ?? null;
    const state = (event.state as AgentState | null) ?? null;
    const agent = (event.agent as KnownAgent | null | undefined) ?? null;
    const detail = (event.detail as string | null | undefined) ?? null;
    if (sessionId) {
      // Notify BEFORE applying so the engine sees the true previous state. Use
      // the event's announced identity when present (an identity announce), else
      // the session's already-known agent (a pure state report carries none).
      noteAgentStateTransition(
        sessionId,
        state,
        agent ?? get(sessionAgents)[sessionId] ?? null,
        detail,
        false,
      );
      applyAgentState(sessionId, state);
      applySessionAgent(sessionId, agent);
    }
  }
  if (event.type === "output-active") {
    const sessionId = event.session_id as Id;
    const active = Boolean(event.active);
    sessionOutputActive.update((current) =>
      current[sessionId] === active ? current : { ...current, [sessionId]: active },
    );
  }
  if (event.type === "session-command") {
    const sessionId = event.session_id as Id;
    const command = (event.command as string | null) ?? null;
    sessionCommands.update((current) => ({ ...current, [sessionId]: command }));
  }
  if (event.type === "job-progress") {
    applyJobProgress(
      event.job_id as Id,
      event.status as JobStatus,
      (event.message as string | null) ?? null,
      (event.kind as string | null) ?? null,
    );
  }
  if (event.type === "composite-job-progress") {
    applyCompositeJobProgress(
      event.job_id as Id,
      event.worktree_id as Id,
      event.kind as CompositeJobKind,
      event.step as CompositeStep,
      event.phase as StepPhase,
    );
  }
  if (event.type === "job-completed") {
    // `completeJob` also resolves the composite chain's per-worktree display
    // state (clear on success, park the oxide failure on a mid-chain failure).
    completeJob(event.job_id as Id, event.response as Response);
  }
  if (event.type === "session-opened") {
    const session = event.session as Session;
    sessions.update((items) => upsert(items, session));
    // Replay the daemon-owned state, announced identity, and output gate on
    // attach so a late-joining window is immediately correct (ADR 0011).
    const state = (event.agent_state as AgentState | null | undefined) ?? null;
    const agent = (event.agent as KnownAgent | null | undefined) ?? null;
    const detail = (event.agent_detail as string | null | undefined) ?? null;
    const outputActive = Boolean(event.output_active);
    // Replayed state primes the notification baseline (and turn-start clock for
    // an attach mid-turn) WITHOUT notifying — a catching-up window must never
    // ping for state it merely learned about (ADR 0011 attach replay).
    noteAgentStateTransition(session.id, state, agent, detail, true);
    applyAgentState(session.id, state);
    applySessionAgent(session.id, agent);
    sessionOutputActive.update((current) =>
      current[session.id] === outputActive ? current : { ...current, [session.id]: outputActive },
    );
    activeSessionId.update((current) => current ?? session.id);
    // Reset the ring + (re)register the output channel. On a reconnect the
    // daemon replays the full scrollback right after this event, so the
    // reset keeps that replay from duplicating the prior bytes.
    openSessionOutput(session.id);
  }
  if (event.type === "session-closed") {
    const sessionId = event.session_id as Id;
    // Drop the session locally; per-worktree/project badges derive from the
    // remaining sessions' daemon-owned agent state.
    sessions.update((items) => items.filter((s) => s.id !== sessionId));
    activeSessionId.update((current) => (current === sessionId ? null : current));
    agentStates.update((current) => {
      const next = { ...current };
      delete next[sessionId];
      return next;
    });
    sessionAgents.update((current) => {
      if (!(sessionId in current)) return current;
      const next = { ...current };
      delete next[sessionId];
      return next;
    });
    sessionOutputActive.update((current) => {
      if (!(sessionId in current)) return current;
      const next = { ...current };
      delete next[sessionId];
      return next;
    });
    sessionCommands.update((current) => {
      const next = { ...current };
      delete next[sessionId];
      return next;
    });
    closeSessionOutput(sessionId);
    // Drop the session's notification bookkeeping (turn-start clock) so the
    // per-session maps don't leak as sessions come and go.
    forgetSessionNotifications(sessionId);
  }
  if (event.type === "project-removed") {
    // A project was forgotten (possibly by another window). Drop it and
    // its worktrees locally; the daemon also broadcasts session-closed for
    // any sessions it killed, so those are pruned above.
    const projectId = event.project_id as Id;
    const removedWorktreeIds = new Set(
      get(worktrees)
        .filter((w) => w.project_id === projectId)
        .map((w) => w.id),
    );
    if (get(selectedProjectId) === projectId) {
      const remaining = get(projects).filter((p) => p.id !== projectId);
      selectedProjectId.set(remaining.length > 0 ? remaining[0].id : null);
      selectedWorktreeId.set(null);
    } else if (removedWorktreeIds.has(get(selectedWorktreeId) as Id)) {
      selectedWorktreeId.set(null);
    }
    projects.update((items) => items.filter((p) => p.id !== projectId));
    worktrees.update((items) => items.filter((w) => w.project_id !== projectId));
    // Drop the removed project's scope tag so the map never retains a stale
    // owner (and a re-added project with the same id can't inherit it).
    projectScopes.update((current) => omitKey(current, projectId));
    // Evict every vanished worktree's per-worktree state (working-tree AND commit
    // diff caches, plus the PR-freshness maps), which are keyed per worktree and
    // would otherwise leak until disposeDaemon. We sweep directly rather than call
    // removeWorktreeLocal per worktree: this handler already owns selection reset
    // above, and the daemon broadcasts session-closed for any killed sessions
    // (pruned in the session-closed branch) — so sweepWorktreeCaches covers the
    // remaining per-worktree leaks (the same set removeWorktreeLocal sweeps).
    for (const id of removedWorktreeIds) sweepWorktreeCaches(id);
  }
}

// ---- connection lifecycle -------------------------------------------------

let unlisteners: UnlistenFn[] = [];
let booted = false;

// Set up the three daemon event subscriptions, then run the connect handshake
// and load the initial snapshot. `connect_daemon` spawns a daemon if none is
// listening, so this doubles as the recovery path after a quit/reboot.
export async function initDaemon(): Promise<void> {
  if (booted) return;
  booted = true;

  connection.set("connecting");
  try {
    unlisteners.push(
      await listen<HitchEvent>("hitch-event", (message) => applyHitchEvent(message.payload)),
    );

    unlisteners.push(
      await listen<{ reason: string }>("hitch-disconnected", (message) => {
        connection.set("offline");
        error.set(message.payload.reason);
        // The daemon link dropped; ephemeral Jobs cannot survive it (ADR 0008).
        failAllJobs(message.payload.reason);
      }),
    );

    // Daemon Status drives the four-state model + the derived connection. The
    // Rust side pushes this on every transition (starting/running/unreachable/
    // failed) with a log-sourced reason on failure (ADR 0009).
    unlisteners.push(
      await listen<{ status: DaemonStatus; reason: string | null; log_path: string }>(
        "hitch-status",
        (message) => {
          applyDaemonStatus(message.payload.status, message.payload.reason);
          daemonLogPath.set(message.payload.log_path);
        },
      ),
    );

    // Auto-recovery re-attached the socket; re-snapshot so projects/worktrees/
    // sessions reflect the live daemon (sessions also replay via events).
    unlisteners.push(
      await listen("hitch-reconnected", () => {
        void refreshAll().catch((err) => error.set(toMessage(err)));
        // A reconnect mid-chain must restore the selected worktree's in-flight
        // composite chain display from the daemon's active Jobs (chains are
        // ephemeral across a daemon RESTART, but survive a transient socket drop).
        void refreshActiveJobs();
      }),
    );

    // Seed the log path up front so "View log" works even if the first connect
    // fails (the status events also carry it).
    try {
      const snapshot = await invoke<{
        status: DaemonStatus;
        reason: string | null;
        log_path: string;
      }>("get_daemon_status");
      applyDaemonStatus(snapshot.status, snapshot.reason);
      daemonLogPath.set(snapshot.log_path);
    } catch {
      // Non-fatal: the status events populate the path on the next transition.
    }

    await invoke("connect_daemon");
    applyDaemonStatus("running", null);
    // Warm the cached notification-permission decision once on connect (unless
    // the user has notifications off) so the first fire doesn't pay an IPC
    // round-trip. This shows no dialog on desktop — the plugin's desktop
    // permission is an always-granted stub; the macOS prompt (and delivery) only
    // happens when a notification is actually posted from an installed .app. Not
    // awaited: it must not hold up the initial snapshot, and every send guards on
    // the cached decision anyway.
    void primeNotificationPermission();
    await refreshSnapshotAfterConnect();
  } catch (err) {
    applyDaemonStatus("failed", toMessage(err));
  }
}

// Apply a Daemon Status to the stores, deriving the narrower `connection` the
// git-poll guard and the offline banner read. Exported as the seam the
// `hitch-status` listener and the unit tests both drive.
export function applyDaemonStatus(status: DaemonStatus, reason: string | null): void {
  daemonStatus.set(status);
  daemonReason.set(reason);
  // Mirror the local Daemon's liveness onto its tree scope so a collapsed Local
  // header can show status alongside future SSH Host scopes (ADR 0014). Each
  // scope owns its own Daemon Status; Local's tracks `daemonStatus` here.
  daemonScopes.update((scopes) =>
    scopes.map((scope) =>
      scope.id === LOCAL_SCOPE_ID && scope.status !== status ? { ...scope, status } : scope,
    ),
  );
  if (status === "running") {
    connection.set("ready");
    error.set(null);
    startPrStatusPolling();
  } else if (status === "starting") {
    connection.set("connecting");
  } else {
    // unreachable | failed
    connection.set("offline");
    if (reason) error.set(reason);
    failAllJobs(reason ?? status);
    stopPrStatusPolling();
  }
}

export function disposeDaemon(): void {
  unlisteners.forEach((unlisten) => unlisten());
  unlisteners = [];
  stopGitStatusPolling();
  stopPrStatusPolling();
  for (const sessionId of Array.from(channels.keys())) {
    closeSessionOutput(sessionId);
  }
  rings.clear();
  diffCache.clear();
  diffRequestSeq.clear();
  diffCacheWriteSeq.clear();
  commitDiffCache.clear();
  commitDiffInFlight.clear();
  commitLog.set(EMPTY_COMMIT_LOG);
  commitLogHeadId = null;
  compositeChains.set({});
  // Reset scope state to just the Local scope (no projects tagged). SSH Host
  // scopes (issue #27) are re-derived from saved hosts on the next boot.
  projectScopes.set({});
  daemonScopes.set([{ ...LOCAL_SCOPE }]);
  booted = false;
}

// Re-run the connect handshake on demand — the manual "daemon went away" path.
// Auto-recovery (ADR 0009) usually handles this, but the button stays for a
// daemon the GUI has given up on (crash-loop `failed`).
export async function reconnect(): Promise<void> {
  error.set(null);
  applyDaemonStatus("starting", null);
  try {
    await invoke("connect_daemon");
    applyDaemonStatus("running", null);
    await refreshSnapshotAfterConnect();
  } catch (err) {
    applyDaemonStatus("failed", toMessage(err));
  }
}

// Restart the daemon from the UI (status popover / tray "Restart daemon").
export async function restartDaemon(): Promise<void> {
  error.set(null);
  applyDaemonStatus("starting", null);
  try {
    await invoke("restart_daemon_command");
    applyDaemonStatus("running", null);
    await refreshSnapshotAfterConnect();
  } catch (err) {
    applyDaemonStatus("failed", toMessage(err));
  }
}

// Fetch the daemon log tail for the status popover.
export async function fetchDaemonLogTail(lines = 200): Promise<string | null> {
  try {
    return await invoke<string | null>("get_daemon_log_tail", { lines });
  } catch {
    return null;
  }
}

// Open the daemon log file in the OS default viewer (status popover / tray).
export async function openDaemonLog(): Promise<void> {
  const path = get(daemonLogPath);
  if (!path) return;
  try {
    const { openPath } = await import("@tauri-apps/plugin-opener");
    await openPath(path);
  } catch (err) {
    error.set(toMessage(err));
  }
}

// ---- git status load ------------------------------------------------------

export async function loadGitStatus(worktreeId: Id): Promise<GitStatus> {
  const requestSeq = ++statusRequestSeq;
  const response = await daemonRequest<Response & { status: GitStatus }>({
    type: "git-status",
    worktree_id: worktreeId,
  });
  // A slow status response from a previous worktree/poll must not replace newer
  // UI state. This matters when agents edit files while the user stages changes.
  if (requestSeq === statusRequestSeq && get(gitWorktreeId) === worktreeId) {
    gitStatus.set(response.status);
    // History freshness rides the 1s status backbone (no new poller). When the
    // selected worktree's HEAD sha moves (e.g. an Agent committed in a PTY),
    // refresh the log IN PLACE — but only when it's worth it: HISTORY is the
    // visible rail view, or the log store is already showing this worktree (stale
    // rows the user may switch back to). A non-visible, never-loaded log waits for
    // its first open. The shape `head_commit_id` is optional for rolling upgrades.
    //
    // refreshCommitLog (not loadCommitLog) preserves the paginated row count and,
    // via the sha-keyed each, every surviving row's DOM node — so a commit landing
    // mid-click never tears down the row the user is pressing (which would drop the
    // synthesized `click` and leave late rows un-openable; see refreshCommitLog).
    const headId = response.status.head_commit_id ?? null;
    if (headId !== commitLogHeadId) {
      commitLogHeadId = headId;
      const logShowsThisWorktree = get(commitLog).worktreeId === worktreeId;
      if (get(railView) === "history" || logShowsThisWorktree) {
        void refreshCommitLog();
      }
    }
  }
  dirtyWorktrees.update((current) =>
    current[worktreeId] === response.status.dirty
      ? current
      : { ...current, [worktreeId]: response.status.dirty },
  );
  worktreeLineStats.update((current) => {
    const next = {
      additions: response.status.additions,
      deletions: response.status.deletions,
    };
    const previous = current[worktreeId];
    if (
      previous?.additions === next.additions &&
      previous?.deletions === next.deletions
    ) {
      return current;
    }
    return { ...current, [worktreeId]: next };
  });
  return response.status;
}

// Fetch the PR for a worktree's branch and store it. On-demand only (worktree
// switch + after git ops). A seq guard drops a slow response once the selected
// worktree has moved on, mirroring loadGitStatus. Failures clear to `null` so
// the UI falls back to offering Create-PR rather than getting stuck.
export async function loadPrStatus(worktreeId: Id): Promise<void> {
  const freshnessSeq = ++prByWorktreeSeq;
  // Mark a lookup for this worktree as in flight so a slower, older project-wide
  // response can't clobber it before it resolves (see prByWorktreeStarted).
  prByWorktreeStarted.set(worktreeId, freshnessSeq);
  try {
    const response = await runJob<Response & { pr: PrInfo | null }>(
      { type: "pr-status", worktree_id: worktreeId },
      "pr-status",
    );
    // The result is authoritative for `worktreeId` regardless of which worktree
    // is selected now, so always feed the per-worktree map; the selected
    // worktree's single `prInfo` is gated by the same per-worktree freshness
    // guard so an unrelated project's batched lookup can't drop this result.
    const applied = writePrByWorktree(worktreeId, response.pr ?? null, freshnessSeq);
    if (applied && get(gitWorktreeId) === worktreeId) {
      prInfo.set(response.pr ?? null);
    }
  } catch {
    // A failed lookup is authoritative for this worktree only if no newer lookup
    // has started or landed. Keep the selected action state and sidebar chip in
    // lockstep by clearing both through the same per-worktree freshness guard.
    const applied = writePrByWorktree(worktreeId, null, freshnessSeq);
    if (applied && get(gitWorktreeId) === worktreeId) {
      prInfo.set(null);
    }
  }
}

// PR status is one `gh pr list` per project, so it's network-priced; refreshAll
// can fire often (after every git op), so throttle per project. `gh pr list`
// rarely changes faster than this between refreshes, and a worktree switch still
// fetches its own fresh status via loadPrStatus.
const PR_STATUS_MIN_INTERVAL_MS = 20_000;
const lastProjectPrFetch = new Map<Id, number>();
// Projects with a `project-pr-statuses` job still pending. `runJob` has no
// timeout, so a hung/slow `gh pr list` keeps its promise open indefinitely; the
// forced periodic poll bypasses the throttle, so without this guard every tick
// would start another daemon job and another `gh` process for the same repo.
const projectPrInFlight = new Set<Id>();

// Populate the PR chip for EVERY worktree of a project from a single batched
// lookup, so chips appear without visiting each worktree. Best-effort and
// fire-and-forget from refreshAll; a failure just clears the throttle so the
// next refresh can retry sooner.
export async function loadProjectPrStatuses(
  projectId: Id,
  options: { force?: boolean } = {},
): Promise<void> {
  const now = Date.now();
  if (!options.force && now - (lastProjectPrFetch.get(projectId) ?? 0) < PR_STATUS_MIN_INTERVAL_MS) {
    return;
  }
  // A previous lookup for this project is still pending (slow/hung `gh`). Forced
  // polls bypass the throttle, so without this we'd keep stacking jobs.
  if (projectPrInFlight.has(projectId)) {
    return;
  }
  projectPrInFlight.add(projectId);
  lastProjectPrFetch.set(projectId, now);
  // Stamp before awaiting: a project-wide response that started before a newer
  // per-worktree `loadPrStatus` must not clobber that fresher status. The same
  // stamp gates the selected worktree's single `prInfo`, mirroring loadPrStatus.
  const freshnessSeq = ++prByWorktreeSeq;
  try {
    const response = await runJob<
      Response & { statuses: { worktree_id: Id; pr: PrInfo | null }[] }
    >({ type: "project-pr-statuses", project_id: projectId }, "pr-status");
    const selectedId = get(gitWorktreeId);
    for (const status of response.statuses) {
      const applied = writePrByWorktree(status.worktree_id, status.pr ?? null, freshnessSeq);
      // Keep the selected worktree's action state (Create vs Open PR) in step with
      // its sidebar chip; both now flow from the same batched lookup. Gated by the
      // per-worktree freshness guard so a slower batched response can't regress a
      // newer per-worktree result.
      if (applied && selectedId === status.worktree_id) {
        prInfo.set(status.pr ?? null);
      }
    }
  } catch {
    lastProjectPrFetch.delete(projectId);
  } finally {
    projectPrInFlight.delete(projectId);
  }
}

function stopGitStatusPolling(): void {
  if (statusPollTimer) clearInterval(statusPollTimer);
  statusPollTimer = null;
  statusPollInFlight = false;
}

function pollSelectedGitStatus(worktreeId: Id): void {
  if (statusPollInFlight || get(connection) !== "ready" || get(gitBusy)) return;
  if (get(gitWorktreeId) !== worktreeId) return;
  statusPollInFlight = true;
  void loadGitStatus(worktreeId)
    .catch(() => {})
    .finally(() => {
      statusPollInFlight = false;
    });
}

function startGitStatusPolling(worktreeId: Id): void {
  stopGitStatusPolling();
  statusPollTimer = setInterval(() => pollSelectedGitStatus(worktreeId), STATUS_POLL_MS);
}

// Refresh PR chips for every git-backed project. The periodic poll forces past
// the refreshAll throttle (it is itself paced by PR_POLL_MS); the focus refresh
// does not, so rapid window switching can't hammer `gh`.
function pollAllProjectPrStatuses(options: { force?: boolean } = {}): void {
  if (get(connection) !== "ready") return;
  for (const project of get(projects)) {
    if (project.kind === "git-backed") {
      void loadProjectPrStatuses(project.id, options);
    }
  }
}

function startPrStatusPolling(): void {
  stopPrStatusPolling();
  prPollTimer = setInterval(() => {
    // Don't poll a backgrounded app: the focus refresh below catches us up the
    // moment the user returns, so a hidden window spawns no `gh` processes.
    if (typeof document !== "undefined" && document.hidden) return;
    pollAllProjectPrStatuses({ force: true });
  }, PR_POLL_MS);
  if (typeof window !== "undefined" && !prFocusHandler) {
    prFocusHandler = () => pollAllProjectPrStatuses();
    window.addEventListener("focus", prFocusHandler);
  }
}

function stopPrStatusPolling(): void {
  if (prPollTimer) clearInterval(prPollTimer);
  prPollTimer = null;
  if (typeof window !== "undefined" && prFocusHandler) {
    window.removeEventListener("focus", prFocusHandler);
    prFocusHandler = null;
  }
}

function statusAfterStage(status: FileStatus): FileStatus {
  return status === "untracked" ? "added" : status;
}

function statusAfterUnstage(status: FileStatus): FileStatus {
  return status === "added" ? "untracked" : status;
}

function optimisticallySetFilesStaged(
  worktreeId: Id,
  paths: string[],
  staged: boolean,
): void {
  const selected = new Set(paths);
  gitStatus.update((current) => {
    if (!current || current.worktree_id !== worktreeId) return current;
    let changed = false;
    // A partially-staged file has two rows (staged + unstaged). Flipping a side
    // merges it into the other side's row, so dedupe by (path, staged) — keeping
    // the first occurrence preserves the flipped row's post-stage/unstage status
    // (rows arrive staged-side first, matching the daemon's emit order).
    const seen = new Set<string>();
    const files: ChangedFile[] = [];
    for (const file of current.files) {
      let next = file;
      if (selected.has(file.path) && file.staged !== staged) {
        next = {
          ...file,
          staged,
          status: staged ? statusAfterStage(file.status) : statusAfterUnstage(file.status),
        };
        changed = true;
      }
      const key = `${next.staged}\0${next.path}`;
      if (seen.has(key)) {
        changed = true;
        continue;
      }
      seen.add(key);
      files.push(next);
    }
    return changed ? { ...current, dirty: files.length > 0, files } : current;
  });
  dirtyWorktrees.update((current) =>
    current[worktreeId] === true ? current : { ...current, [worktreeId]: true },
  );
}

// ---- request actions ------------------------------------------------------

// The form/confirmation dialogs (add-project, clone, create/remove worktree,
// create-PR) throw on failure so they can surface the error inline and only
// dismiss on success. The fire-and-forget actions below (open/rename/close
// session, stage, commit, push) instead swallow into the `error` store.
export async function addProject(root: string): Promise<void> {
  const trimmed = root.trim();
  if (!trimmed) return;
  await daemonRequest({ type: "add-project", root: trimmed });
  await refreshAll();
}

// Open the native folder picker and add the chosen directory as a project.
// This stays the primary local add-project flow; the separate dialog fallback
// handles manual path entry when the picker is unavailable or unsuitable.
// Cancelling is a silent no-op; failures surface in the `error` store.
export async function pickAndAddProject(): Promise<void> {
  try {
    const picked = await open({
      directory: true,
      multiple: false,
      title: "Add a project folder",
    });
    if (typeof picked !== "string") return;
    await addProject(picked);
  } catch (err) {
    error.set(toMessage(err));
  }
}

export async function cloneProject(
  remoteUrl: string,
  destination: string,
  name: string | null = null,
): Promise<void> {
  await runJob(
    {
      type: "clone-project",
      remote_url: remoteUrl.trim(),
      destination: destination.trim(),
      name: name?.trim() || null,
    },
    "clone",
  );
  await refreshAll();
}

export async function removeProject(projectId: Id, force: boolean): Promise<void> {
  await daemonRequest({
    type: "remove-project",
    project_id: projectId,
    force,
  });
  if (get(selectedProjectId) === projectId) {
    selectedProjectId.set(null);
    selectedWorktreeId.set(null);
  }
  await refreshAll();
}

export async function listBranches(projectId: Id): Promise<BranchSummary[]> {
  const response = await daemonRequest<Response & { branches: BranchSummary[] }>({
    type: "list-branches",
    project_id: projectId,
  });
  return response.branches;
}

export async function createWorktree(
  projectId: Id,
  branch: string,
  base: string | null = null,
  mode: "new-branch" | "existing-branch" = "new-branch",
): Promise<Worktree | null> {
  const trimmed = branch.trim();
  if (!trimmed) return null;
  const response = await runJob<Response & { worktrees: Worktree[] }>(
    {
      type: "create-worktree",
      project_id: projectId,
      branch: trimmed,
      base,
      mode,
    },
    "create-worktree",
  );
  const created = response.worktrees[0] ?? null;
  if (created) {
    worktrees.update((items) => upsert(items, created));
    selectedWorktreeId.set(created.id);
  }
  return created;
}

// `force` overrides the daemon's dirty-worktree / live-session guards — the
// remove dialog confirms exactly those cases before calling. `delete_branch`
// is gated by ADR 0001 (only when merged); the frozen contract carries no
// merge status, so the dialog keeps that option disabled and passes `false`.
export async function removeWorktree(
  worktreeId: Id,
  deleteBranch: boolean,
  force: boolean,
): Promise<void> {
  await daemonRequest({
    type: "remove-worktree",
    worktree_id: worktreeId,
    delete_branch: deleteBranch,
    force,
  });
  removeWorktreeLocal(worktreeId);
}

// Last grid an active Terminal successfully fitted to, in cols/rows. Used to
// open the NEXT session's PTY at the right size so it doesn't reflow on first
// fit. Updated by Terminal.svelte after every successful fit; null until the
// first terminal has rendered (the very first open then estimates from the DOM).
let lastTerminalSize: { cols: number; rows: number } | null = null;

// Terminal.svelte calls this after a successful fit with its live term.cols/rows.
export function recordTerminalSize(cols: number, rows: number): void {
  if (cols > 0 && rows > 0) lastTerminalSize = { cols, rows };
}

// The xterm config and panel inset in Terminal.svelte. Kept here so the
// offscreen measuring span uses the same font as the real terminal and subtracts
// the wrapper padding before dividing the grid. The font stack comes from the
// shared settings helper (user-picked family + built-in fallback), the same
// source Terminal.svelte renders with.
const TERM_FONT_SIZE_PX = 13;
// `.terminal` padding from Terminal.svelte (`padding: 14px 16px 4px`).
const TERM_PADDING_X_PX = 16;
const TERM_PADDING_TOP_PX = 14;
const TERM_PADDING_BOTTOM_PX = 4;
// Last-resort grid when nothing on the page is measurable (e.g. opening a
// session before any view has laid out). A sane terminal-ish default.
const FALLBACK_COLS = 120;
const FALLBACK_ROWS = 32;

// Estimate the grid a freshly-opened terminal will fit to, so its PTY spawns at
// (close to) that size. We measure the visible center view area and divide by
// the cell size derived from the xterm font via an offscreen probe `<span>`.
// This is only the FIRST-open path; every subsequent open reuses the exact
// `lastTerminalSize` recorded by a live terminal, so the estimate just needs to
// be in the right ballpark to avoid a jarring reflow.
function estimateInitialSize(): { cols: number; rows: number } {
  if (typeof document === "undefined") {
    return { cols: FALLBACK_COLS, rows: FALLBACK_ROWS };
  }
  // The center pane is where terminals render; fall back to body if absent.
  const view =
    document.querySelector(".view") ??
    document.querySelector(".center") ??
    document.body;
  const rect = view?.getBoundingClientRect();
  if (!rect || rect.width === 0 || rect.height === 0) {
    return { cols: FALLBACK_COLS, rows: FALLBACK_ROWS };
  }

  // Offscreen probe with the terminal's exact font. `ch`-style width: measure a
  // wide run of a monospace glyph and divide, so sub-pixel advance is averaged.
  const probe = document.createElement("span");
  probe.style.position = "absolute";
  probe.style.visibility = "hidden";
  probe.style.whiteSpace = "pre";
  probe.style.fontFamily = terminalFontStack(get(terminalFontFamily));
  probe.style.fontSize = `${TERM_FONT_SIZE_PX}px`;
  probe.style.lineHeight = "normal";
  probe.textContent = "0".repeat(100);
  document.body.appendChild(probe);
  const cellWidth = probe.getBoundingClientRect().width / 100;
  const cellHeight = probe.getBoundingClientRect().height;
  probe.remove();

  if (cellWidth <= 0 || cellHeight <= 0) {
    return { cols: FALLBACK_COLS, rows: FALLBACK_ROWS };
  }
  // Subtract the `.terminal` wrapper padding before dividing by the cell size.
  const usableWidth = rect.width - TERM_PADDING_X_PX * 2;
  const usableHeight = rect.height - TERM_PADDING_TOP_PX - TERM_PADDING_BOTTOM_PX;
  const cols = Math.max(1, Math.floor(usableWidth / cellWidth));
  const rows = Math.max(1, Math.floor(usableHeight / cellHeight));
  return { cols, rows };
}

// `command` is an argv (e.g. ["claude"]); null spawns the default shell. This
// mirrors the daemon's OpenSession.command (Option<Vec<String>>) contract.
// `cols`/`rows` carry the initial grid so the PTY spawns at the right size and
// the terminal doesn't visibly reflow on its first fit.
export async function openSession(
  parent: SessionParent,
  name: string,
  command: string[] | null = null,
): Promise<Session | null> {
  try {
    error.set(null);
    const { cols, rows } = lastTerminalSize ?? estimateInitialSize();
    const response = await daemonRequest<Response & { session: Session }>({
      type: "open-session",
      parent,
      name: name.trim() || "shell",
      command,
      cols,
      rows,
    });
    activeSessionId.set(response.session.id);
    if (command?.[0]) {
      sessionCommands.update((current) => ({ ...current, [response.session.id]: command[0] }));
    }
    return response.session;
  } catch (err) {
    error.set(toMessage(err));
    return null;
  }
}

export async function renameSession(session: Session, name: string): Promise<void> {
  const next = name.trim();
  if (!next || next === session.name) return;
  try {
    error.set(null);
    await daemonRequest({
      type: "rename-session",
      session_id: session.id,
      name: next,
    });
    sessions.update((items) =>
      items.map((item) => (item.id === session.id ? { ...item, name: next } : item)),
    );
  } catch (err) {
    error.set(toMessage(err));
  }
}

export async function closeSession(session: Session): Promise<void> {
  try {
    error.set(null);
    await daemonRequest({
      type: "close-session",
      session_id: session.id,
      kill_process: true,
    });
    sessions.update((items) => items.filter((item) => item.id !== session.id));
    closeSessionOutput(session.id);
  } catch (err) {
    error.set(toMessage(err));
  }
}

// Close whichever tab is active (Cmd+W), matching how SessionTabs' per-tab ×
// closes each kind: an active diff tab is dropped by path via `closeDiff`
// (which re-activates a neighbor); an active session is closed via
// `closeSession` (kill + drop, no confirm — the same flow the × button and the
// context menu use). No-op when nothing is active.
export function closeActiveTab(): void {
  if (get(diffActive)) {
    const path = get(activeDiffPath);
    if (path !== null) closeDiff(path);
    return;
  }
  const session = get(activeSession);
  if (session) void closeSession(session);
}

export async function resizeSession(
  sessionId: Id,
  cols: number,
  rows: number,
): Promise<void> {
  try {
    await daemonRequest({ type: "resize-session", session_id: sessionId, cols, rows });
  } catch (err) {
    // Resize is best-effort: NEVER throw, so keystrokes keep flowing even if
    // the PTY exited mid-resize. But a dropped final size is a real bug (the
    // child renders at the wrong grid), so log it — a silent swallow hid lost
    // resizes. The debounce trailing edge still chains a repaint afterward.
    console.warn(`resize-session failed for ${sessionId} (${cols}x${rows})`, err);
  }
}

// Ask the daemon to force the session's PTY child to redraw a clean full frame
// (it replies with an Ack). Used right after a settled resize and on tab
// re-activation so a TUI like Claude Code repaints crisply at the new/current
// grid. Best-effort: a missing or dead PTY must never break the UI, so all
// errors are swallowed. Emits the agreed `{ type, session_id }` contract.
export async function repaintSession(sessionId: Id): Promise<void> {
  try {
    const request: RepaintSessionRequest = {
      type: "repaint-session",
      session_id: sessionId,
    };
    await daemonRequest(request);
  } catch {
    // Repaint is best-effort; a missing/dead PTY must not break the UI.
  }
}

// Trailing debounce for the daemon resize notification, keyed per session.
// xterm's local fit() reflows smoothly on every ResizeObserver tick, but the
// PTY child only needs the FINAL size: dragging the window or toggling a panel
// fires a storm of ticks, and telling the child about each intermediate size
// floods it with SIGWINCH and garbles a running TUI (vim/lazygit). We coalesce
// those into ONE `resize-session` request that fires ~70 ms after the size
// settles. Per session so two visible terminals don't share a timer.
const RESIZE_DEBOUNCE_MS = 70;
const resizeTimers = new Map<Id, ReturnType<typeof setTimeout>>();

export function resizeSessionDebounced(
  sessionId: Id,
  cols: number,
  rows: number,
): void {
  const existing = resizeTimers.get(sessionId);
  if (existing) clearTimeout(existing);
  resizeTimers.set(
    sessionId,
    setTimeout(() => {
      resizeTimers.delete(sessionId);
      // Lossless settle: the final size always reaches the daemon, and once it
      // has LANDED we force a repaint so the child redraws clean at the new
      // grid. `.finally` runs the repaint even if the resize errored — the
      // settled-frame repaint is the whole point and must not be skipped.
      void resizeSession(sessionId, cols, rows).finally(() => {
        void repaintSession(sessionId);
      });
    }, RESIZE_DEBOUNCE_MS),
  );
}

// Drop any pending debounced resize for a session that is going away, so its
// trailing timer can't fire a request against a dead PTY.
function clearResizeDebounce(sessionId: Id): void {
  const existing = resizeTimers.get(sessionId);
  if (existing) {
    clearTimeout(existing);
    resizeTimers.delete(sessionId);
  }
}

export function sendInput(sessionId: Id, data: string): void {
  void invoke("send_session_input", { sessionId, data });
}

// Write a tab's fetched text into the open set, if a tab for `path` still
// exists. A no-op once the tab has been closed (so a late fetch can't resurrect
// a dismissed tab).
function setDiffTabText(path: string, text: string | null): void {
  diffTabs.update((tabs) =>
    tabs.some((tab) => tab.path === path)
      ? tabs.map((tab) => (tab.path === path ? { ...tab, text } : tab))
      : tabs,
  );
}

// `activate` opens the diff as the center view (a user clicking a changed
// file). The keep-in-sync refresh from staging passes `false` so re-fetching a
// diff never yanks the view away from a terminal the user is looking at. Opening
// a file that's already in the strip re-uses its tab (no duplicates) and just
// refreshes its text; a new file appends a tab.
export async function viewDiff(path: string, activate = true, staged?: boolean): Promise<void> {
  const worktreeId = get(gitWorktreeId);
  if (!worktreeId) return;
  const cacheKey = diffCacheKey(worktreeId, path, staged);
  const requestKey = diffTabFreshnessKey(worktreeId, path);
  const requestSeq = nextDiffRequestSeq(worktreeId, "single", path);
  const writeSeq = nextDiffCacheWriteSeq(cacheKey);

  const cached = diffCache.get(cacheKey) ?? null;
  diffTabs.update((tabs) =>
    tabs.some((tab) => tab.path === path)
      ? tabs.map((tab) => (tab.path === path ? { ...tab, text: cached, staged } : tab))
      : [...tabs, { path, text: cached, staged }],
  );
  if (activate) {
    activeDiffPath.set(path);
    diffActive.set(true);
  } else if (get(activeDiffPath) === null) {
    // First tab opened by a background refresh still needs an active target so
    // the view has something to show once the user switches to it.
    activeDiffPath.set(path);
  }

  // The tab is live (still open) and this is the freshest request for the visible path.
  const isLatest = () =>
    diffRequestSeq.get(requestKey) === requestSeq &&
    get(gitWorktreeId) === worktreeId &&
    get(diffTabs).some((tab) => tab.path === path);

  try {
    const request: GitDiffRequest = {
      type: "git-diff",
      worktree_id: worktreeId,
      path,
      ...(staged === undefined ? {} : { staged }),
      ...diffOptionFields(),
    };
    const response = await daemonRequest<Response & { diff: { diff: string } }>(request);
    writeFreshDiffCache(cacheKey, writeSeq, response.diff.diff);
    if (isLatest()) setDiffTabText(path, response.diff.diff);
  } catch (err) {
    error.set(toMessage(err));
    if (isLatest()) setDiffTabText(path, null);
  }
}

// Fetch one file's unified diff for the all-changes fan-out. It has its own
// freshness key so it cannot cancel a single-file tab request for the same path.
// Cache writes are separately guarded so an older response cannot replace newer
// cached text.
async function fetchFileDiff(
  worktreeId: Id,
  path: string,
  staged: boolean,
): Promise<string | null> {
  const cacheKey = diffCacheKey(worktreeId, path, staged);
  const cached = diffCache.get(cacheKey);
  if (cached !== undefined) return cached;
  const requestSeq = nextDiffRequestSeq(worktreeId, "all", path, staged);
  const requestKey = diffFreshnessKey(worktreeId, "all", path, staged);
  const writeSeq = nextDiffCacheWriteSeq(cacheKey);
  try {
    const request: GitDiffRequest = {
      type: "git-diff",
      worktree_id: worktreeId,
      path,
      staged,
      ...diffOptionFields(),
    };
    const response = await daemonRequest<Response & { diff: { diff: string } }>(request);
    const wrote = writeFreshDiffCache(cacheKey, writeSeq, response.diff.diff);
    if (diffRequestSeq.get(requestKey) !== requestSeq) return diffCache.get(cacheKey) ?? null;
    return wrote ? response.diff.diff : diffCache.get(cacheKey) ?? null;
  } catch (err) {
    error.set(toMessage(err));
    return null;
  }
}

// Open (or refresh + activate) the single all-changes tab: one unified view of
// every changed file. The file list is taken from the current git status (staged
// first, then unstaged, matching RightRail), and each diff is fetched through a
// bounded pool via `fetchFileDiff` (reusing `diffCache`). A monotonic seq guards
// older fan-out (e.g. from before a stage/unstage) overwriting a newer one. The
// tab is represented by the `ALL_CHANGES_TAB` sentinel in `diffTabs` so it lives
// in the strip as a peer; its content lives in `allChangesFiles`, not its `text`.
let allChangesSeq = 0;
export async function viewAllChanges(activate = true): Promise<void> {
  const worktreeId = get(gitWorktreeId);
  if (!worktreeId) return;
  const status = get(gitStatus);
  const files = status && status.worktree_id === worktreeId ? status.files : [];
  // Staged before unstaged, preserving each group's existing order.
  const ordered = [
    ...files.filter((file) => file.staged),
    ...files.filter((file) => !file.staged),
  ];
  lastAllChangesSig = allChangesStatusSignature(worktreeId, files);

  const seq = ++allChangesSeq;
  // Seed the rows (text `null` = loading) so the view paints immediately, reusing
  // any cached diff text up front to avoid a flash of "Loading" for warm files.
  allChangesFiles.set(
    ordered.map((file) => ({
      path: file.path,
      staged: file.staged,
      text: diffCache.get(diffCacheKey(worktreeId, file.path, file.staged)) ?? null,
    })),
  );

  diffTabs.update((tabs) =>
    tabs.some((tab) => tab.path === ALL_CHANGES_TAB)
      ? tabs
      : [...tabs, { path: ALL_CHANGES_TAB, text: null }],
  );
  if (activate) {
    activeDiffPath.set(ALL_CHANGES_TAB);
    diffActive.set(true);
  } else if (get(activeDiffPath) === null) {
    activeDiffPath.set(ALL_CHANGES_TAB);
  }

  // This fan-out is still the freshest and its worktree is still selected.
  const isLatest = () => allChangesSeq === seq && get(gitWorktreeId) === worktreeId;

  // Only fetch diffs for rows the user has expanded. A collapsed section isn't
  // rendered, so fetching it is wasted work — and the 1s status poll can flip the
  // signature on pure line-count drift (e.g. typing in an external editor) and
  // evict the diff cache, which would otherwise re-fan every file. Collapsed rows
  // fetch lazily when expanded (`fetchAllChangesRow`); their seeded text (cache
  // hit or `null`) is left in place here. On the initial open every row is
  // collapsed (the default), so nothing fans out until the user expands rows.
  const expanded = ordered.filter((file) => allChangesRowExpanded(file.path, file.staged));

  await forEachBounded(expanded, ALL_CHANGES_DIFF_CONCURRENCY, async (file) => {
    const text = await fetchFileDiff(worktreeId, file.path, file.staged);
    if (!isLatest()) return;
    allChangesFiles.update((rows) =>
      rows.map((row) =>
        row.path === file.path && row.staged === file.staged
          ? {
              ...row,
              text: text ?? diffCache.get(diffCacheKey(worktreeId, file.path, file.staged)) ?? row.text,
            }
          : row,
      ),
    );
  });
}

// Fetch one all-changes row's diff on demand — used when the user expands a
// previously-collapsed section. Reuses the diff cache (so an already-warm row
// renders instantly with no daemon round-trip) and the same freshness guards as
// the fan-out, so a stage/unstage or worktree switch in flight can't repopulate a
// stale row. A no-op for the sentinel-less / wrong-worktree state.
export async function fetchAllChangesRow(path: string, staged: boolean): Promise<void> {
  const worktreeId = get(gitWorktreeId);
  if (!worktreeId) return;
  const seq = allChangesSeq;
  const text = await fetchFileDiff(worktreeId, path, staged);
  if (allChangesSeq !== seq || get(gitWorktreeId) !== worktreeId) return;
  allChangesFiles.update((rows) =>
    rows.map((row) =>
      row.path === path && row.staged === staged
        ? { ...row, text: text ?? diffCache.get(diffCacheKey(worktreeId, path, staged)) ?? row.text }
        : row,
    ),
  );
}

// Expand or collapse every all-changes row at once — the head's expand-all /
// collapse-all toggle. Collapsing just clears the expanded set (collapsed rows
// keep their seeded text; nothing renders them). Expanding marks every current
// row expanded and re-runs `viewAllChanges` so the diffs fetch through its
// bounded pool (warm rows seed from the cache instantly) instead of N unbounded
// per-row on-demand fetches.
export function setAllChangesAllExpanded(expanded: boolean): void {
  if (!expanded) {
    allChangesExpanded.set(new Set());
    return;
  }
  allChangesExpanded.set(
    new Set(get(allChangesFiles).map((row) => allChangesRowKey(row.path, row.staged))),
  );
  void viewAllChanges(false);
}

// Re-fetch every open diff under the current re-diff options. Called when
// `diffIgnoreWhitespace` / `diffContextLines` change: those options are part of
// the cache key, so a fresh fetch under the new key is what surfaces the
// re-shaped diff (old-option text stays cached but unreferenced). Single-file
// tabs re-run `viewDiff` with the `staged` side they were opened with, never
// stealing focus (`activate: false`); the all-changes tab re-fans via
// `viewAllChanges`. Both reuse the existing freshness guards, so this races
// safely with concurrent stage/unstage refreshes.
export function refreshOpenDiffs(): void {
  if (!get(gitWorktreeId)) return;
  const tabs = get(diffTabs);
  for (const tab of tabs) {
    if (tab.path === ALL_CHANGES_TAB) continue;
    // Commit tabs are fed by the commit-diff path (`fetchCommitDiff` /
    // `commitDiffCache`, immutable per-sha snapshots) and rendered client-side
    // via `diffViewOptions` — never re-diffed here. Sending their sentinel
    // (`\0commit:<sha>`) to `viewDiff` would emit it verbatim as a git pathspec,
    // yielding an empty diff that clobbers the tab's text and the diff cache.
    if (isCommitTab(tab.path)) continue;
    void viewDiff(tab.path, false, tab.staged);
  }
  if (tabs.some((tab) => tab.path === ALL_CHANGES_TAB)) void viewAllChanges(false);
}

// Close one diff tab (or, with no argument, all of them) and fall back to the
// active session's terminal when none remain. When the closed tab was the
// active one, the neighbor to its left becomes active (the right one if it was
// first), matching normal editor tab behavior.
export function closeDiff(path?: string): void {
  if (path === undefined) {
    allChangesSeq += 1;
    diffTabs.set([]);
    activeDiffPath.set(null);
    diffActive.set(false);
    allChangesFiles.set([]);
    allChangesExpanded.set(new Set());
    return;
  }
  const tabs = get(diffTabs);
  const index = tabs.findIndex((tab) => tab.path === path);
  if (index === -1) return;
  // Closing the all-changes tab drops its per-file rows and invalidates any
  // in-flight fan-out so late rows cannot repopulate a dismissed tab.
  if (path === ALL_CHANGES_TAB) {
    allChangesSeq += 1;
    allChangesFiles.set([]);
    allChangesExpanded.set(new Set());
  }
  const remaining = tabs.filter((tab) => tab.path !== path);
  diffTabs.set(remaining);
  if (remaining.length === 0) {
    activeDiffPath.set(null);
    diffActive.set(false);
  } else if (get(activeDiffPath) === path) {
    // Prefer the left neighbor; fall back to the new first tab.
    const neighbor = remaining[Math.max(0, index - 1)];
    activeDiffPath.set(neighbor.path);
  }
}

export async function setFilesStaged(
  paths: string[],
  staged: boolean,
  worktreeId: Id | null = get(gitWorktreeId),
): Promise<void> {
  if (!worktreeId || paths.length === 0) return;
  const before = get(gitStatus);
  // Invalidate any in-flight status poll so it cannot briefly undo the
  // immediate stage/unstage feedback.
  statusRequestSeq += 1;
  optimisticallySetFilesStaged(worktreeId, paths, staged);
  gitBusy.set(true);
  try {
    error.set(null);
    await daemonRequest({
      type: staged ? "stage-files" : "unstage-files",
      worktree_id: worktreeId,
      paths,
    });
    gitBusy.set(false);
    void loadGitStatus(worktreeId).catch((err) => error.set(toMessage(err)));
    // Keep any open diff tabs in sync if their file was just (un)staged, without
    // stealing focus from a terminal the user may be looking at. Each tab is
    // re-fetched on its OWN staged side (`tab.staged`), not the operation's
    // `staged`, so an open unstaged-side tab stays unstaged (mirroring
    // `refreshOpenDiffs`). Staging a fully-unstaged file leaving its unstaged
    // diff empty is correct: the tab keeps its side rather than flipping.
    const openTabs = get(diffTabs);
    const affected = new Set(paths);
    for (const path of paths) {
      invalidateDiffCacheVariants(worktreeId, path);
    }
    for (const tab of openTabs) {
      if (tab.path === ALL_CHANGES_TAB) continue;
      if (!affected.has(tab.path)) continue;
      void viewDiff(tab.path, false, tab.staged);
    }
    const openPaths = new Set(openTabs.map((tab) => tab.path));
    // The all-changes tab spans every file, so any (un)stage can change its file
    // set (a row moving group) or counts. Re-fan it without stealing focus; the
    // cache deletes above force its touched files to re-fetch.
    if (openPaths.has(ALL_CHANGES_TAB)) void viewAllChanges(false);
  } catch (err) {
    error.set(toMessage(err));
    if (before?.worktree_id === worktreeId && get(gitWorktreeId) === worktreeId) {
      gitStatus.set(before);
    }
    void loadGitStatus(worktreeId).catch((refreshErr) => error.set(toMessage(refreshErr)));
    // Rethrow so awaiting callers (e.g. the Composer's auto-stage-then-generate)
    // can stop their flow instead of proceeding on a failed stage. The error
    // store + optimistic rollback above still surface the failure on their own,
    // so fire-and-forget callers must `.catch()` (see RightRail).
    throw err;
  } finally {
    gitBusy.set(false);
  }
}

export function setFileStaged(path: string, staged: boolean): Promise<void> {
  return setFilesStaged([path], staged);
}

export async function discardFiles(paths: string[]): Promise<void> {
  const worktreeId = get(gitWorktreeId);
  if (!worktreeId || paths.length === 0) return;
  statusRequestSeq += 1;
  gitBusy.set(true);
  try {
    error.set(null);
    await daemonRequest({
      type: "discard-files",
      worktree_id: worktreeId,
      paths,
    });
    // A discarded file has no diff left to show, so drop its tab.
    for (const path of paths) {
      invalidateDiffCacheVariants(worktreeId, path);
      closeDiff(path);
    }
    await loadGitStatus(worktreeId);
    // The all-changes tab spans the whole working tree, so drop the discarded
    // files from it (or close it if nothing changed remains) once status reloads.
    if (get(diffTabs).some((tab) => tab.path === ALL_CHANGES_TAB)) {
      if ((get(gitStatus)?.files.length ?? 0) === 0) closeDiff(ALL_CHANGES_TAB);
      else void viewAllChanges(false);
    }
  } catch (err) {
    error.set(toMessage(err));
    void loadGitStatus(worktreeId).catch((refreshErr) => error.set(toMessage(refreshErr)));
  } finally {
    gitBusy.set(false);
  }
}

export function discardFile(path: string): Promise<void> {
  return discardFiles([path]);
}

export function discardAllFiles(): Promise<void> {
  // A partially-staged file appears as two rows; send each path only once.
  const paths = [...new Set(get(gitStatus)?.files.map((file) => file.path) ?? [])];
  return discardFiles(paths);
}

export async function commit(
  subject: string,
  body: string | null = null,
  worktreeId: Id | null = get(gitWorktreeId),
): Promise<void> {
  const trimmedSubject = subject.trim();
  const trimmedBody = body?.trim() || null;
  if (!worktreeId || !trimmedSubject) return;
  gitBusy.set(true);
  try {
    error.set(null);
    await daemonRequest({
      type: "commit",
      worktree_id: worktreeId,
      subject: trimmedSubject,
      body: trimmedBody,
    });
    closeDiff();
    await loadGitStatus(worktreeId);
  } catch (err) {
    error.set(toMessage(err));
    throw err;
  } finally {
    gitBusy.set(false);
  }
}

export async function listDraftModels(
  provider: DraftProvider,
  paths: { claudePath?: string; codexPath?: string } = {},
): Promise<string[]> {
  const response = await runJob<Response & { models: string[] }>(
    { type: "list-draft-models", provider, settings: draftDiscoverySettings(provider, paths) },
    "draft-models",
  );
  return response.models;
}

export async function generateCommitDraft(
  worktreeId: Id | null = get(gitWorktreeId),
): Promise<CommitDraft> {
  if (!worktreeId) throw new Error("Select a git worktree first.");
  const response = await runJob<Response & { draft: CommitDraft }>(
    {
      type: "generate-commit-draft",
      worktree_id: worktreeId,
      settings: draftGenerationSettings({ commit: get(draftCommitInstructions) }),
    },
    "commit-draft",
  );
  return response.draft;
}

export async function generatePullRequestDraft(base: string | null): Promise<PullRequestDraft> {
  const worktreeId = get(gitWorktreeId);
  if (!worktreeId) throw new Error("Select a git worktree first.");
  const response = await runJob<Response & { draft: PullRequestDraft }>(
    {
      type: "generate-pull-request-draft",
      worktree_id: worktreeId,
      base: base?.trim() || null,
      settings: draftGenerationSettings({ pr: get(draftPrInstructions) }),
    },
    "pr-draft",
  );
  return response.draft;
}

function draftDiscoverySettings(
  provider: DraftProvider,
  paths: { claudePath?: string; codexPath?: string } = {},
): DraftGenerationSettings | null {
  if (provider === "stub") return null;
  return draftSettingsForProvider(provider, null, paths);
}

// Draft Instructions to fold into a generation request. Only one kind applies
// per request (commit drafts carry the commit guidance; PR drafts the PR
// guidance), so callers pass the relevant raw value; empties are omitted below.
type DraftInstructions = { commit?: string; pr?: string };

function draftGenerationSettings(
  instructions: DraftInstructions = {},
): DraftGenerationSettings | null {
  const provider = get(draftProvider);
  // No explicit desktop choice → omit settings so the daemon keeps its own
  // configured provider/model/path defaults instead of being forced to "stub".
  if (!provider) return null;
  return draftSettingsForProvider(
    provider,
    get(draftModel).trim() || null,
    {},
    instructions,
  );
}

function draftSettingsForProvider(
  provider: DraftProvider,
  model: string | null,
  paths: { claudePath?: string; codexPath?: string } = {},
  instructions: DraftInstructions = {},
): DraftGenerationSettings {
  const claudePath = (paths.claudePath ?? get(draftClaudePath)).trim() || null;
  const codexPath = (paths.codexPath ?? get(draftCodexPath)).trim() || null;
  const settings: DraftGenerationSettings = {
    provider,
    model,
    claude_path: claudePath,
    codex_path: codexPath,
  };
  // Omit instruction fields entirely when blank/whitespace. The daemon treats
  // whitespace as absent too, but keeping empties off the wire keeps the request
  // shape identical to the no-instructions case (and to older callers).
  const commit = instructions.commit?.trim();
  if (commit) settings.commit_instructions = commit;
  const pr = instructions.pr?.trim();
  if (pr) settings.pr_instructions = pr;
  return settings;
}

export async function push(worktreeId: Id | null = get(gitWorktreeId)): Promise<void> {
  if (!worktreeId) return;
  gitBusy.set(true);
  try {
    error.set(null);
    await runJob({ type: "push", worktree_id: worktreeId }, "push");
  } catch (err) {
    error.set(toMessage(err));
    throw err;
  } finally {
    gitBusy.set(false);
  }
}

export async function fetchRemote(worktreeId: Id | null = get(gitWorktreeId)): Promise<void> {
  if (!worktreeId) return;
  gitBusy.set(true);
  try {
    error.set(null);
    await runJob({ type: "fetch", worktree_id: worktreeId }, "fetch");
  } catch (err) {
    error.set(toMessage(err));
    throw err;
  } finally {
    gitBusy.set(false);
  }
}

export async function pull(worktreeId: Id | null = get(gitWorktreeId)): Promise<void> {
  if (!worktreeId) return;
  gitBusy.set(true);
  try {
    error.set(null);
    await runJob({ type: "pull", worktree_id: worktreeId }, "pull");
  } catch (err) {
    error.set(toMessage(err));
    throw err;
  } finally {
    gitBusy.set(false);
  }
}

// Throws on failure so the dialog can surface the error inline (mirrors App.tsx).
export async function createPr(fields: PrFields): Promise<void> {
  const worktreeId = get(gitWorktreeId);
  if (!worktreeId) throw new Error("Select a git worktree first.");
  if (get(gitBusy)) throw new Error("Wait for the current git operation to finish.");
  gitBusy.set(true);
  try {
    const response = await runJob<Response & { url: string }>(
      {
        type: "create-pull-request",
        worktree_id: worktreeId,
        title: fields.title,
        body: fields.body,
        base: fields.base,
        draft: fields.draft,
      },
      "create-pr",
    );
    prUrl.set(response.url);
    // Refresh so the action menu flips from "Create PR" to "Open PR".
    await loadPrStatus(worktreeId);
  } finally {
    gitBusy.set(false);
  }
}

// ---- selection fix-up + cleanup (run once, here, as subscriptions) --------

// Fall back to the first project when nothing is selected yet.
projects.subscribe(($projects) => {
  if (!get(selectedProjectId) && $projects.length > 0) {
    selectedProjectId.set($projects[0].id);
  }
});

// Keep the selected worktree valid for the selected project, but never auto-
// pick one. Plain projects have no worktrees; switching git projects (or
// removing the selected worktree) invalidates the current selection — in both
// cases we clear it to null rather than jumping into `main`. Clicking a project
// then expands it and shows the "choose a worktree" state; the user picks the
// worktree explicitly, so `main` is no longer special on selection.
derived([selectedProject, projectWorktrees], (v) => v).subscribe(
  ([$project, $worktrees]) => {
    const selected = get(selectedWorktreeId);
    if (selected === null) return;
    if ($project?.kind === "plain") {
      selectedWorktreeId.set(null);
      return;
    }
    if ($project && !$worktrees.some((w) => w.id === selected)) {
      selectedWorktreeId.set(null);
    }
  },
);

// Per-parent memory of the last active session id, so switching worktrees
// (or projects) and coming back restores the tab the user had selected
// instead of always snapping to the first session in the new parent.
const lastActiveByParent = new Map<string, Id>();

// Track the current parent so we can detect parent changes vs same-parent
// session list updates (open/close/rename) and only consult the remembered
// id on a real switch.
let lastParentKey: string | null = null;

// On every parent switch, restore the remembered session for that parent
// (if it's still live), else fall back to the first visible one.
selectedParent.subscribe(($parent) => {
  const key = $parent ? parentKey($parent) : null;
  if (key === lastParentKey) return;
  lastParentKey = key;
  if (!$parent) {
    activeSessionId.set(null);
    return;
  }
  const visible = get(sessions).filter((s) => sessionBelongsTo(s, $parent));
  const remembered = lastActiveByParent.get(key!);
  const restore =
    (remembered && visible.some((s) => s.id === remembered) ? remembered : null) ??
    visible[0]?.id ??
    null;
  activeSessionId.set(restore);
});

// Within the current parent, keep the active session valid if a session
// closes or the list changes; don't fight the parent-switch logic above.
visibleSessions.subscribe(($visible) => {
  const active = get(activeSessionId);
  if (active && $visible.some((s) => s.id === active)) return;
  activeSessionId.set($visible[0]?.id ?? null);
});

// Remember the user's active choice per parent so we can restore it later.
activeSessionId.subscribe(($id) => {
  const parent = get(selectedParent);
  if (!parent || !$id) return;
  lastActiveByParent.set(parentKey(parent), $id);
});

// Drop remembered ids for sessions that have closed.
sessions.subscribe(($sessions) => {
  const liveIds = new Set($sessions.map((s) => s.id));
  for (const [key, id] of lastActiveByParent) {
    if (!liveIds.has(id)) lastActiveByParent.delete(key);
  }
});

// Forget agent state, announced identity, output gate, running command, and
// notification bookkeeping for sessions that have closed (or were dropped on a
// reconnect), so stale labels never linger on the tree or tabs and the
// notifications module's per-session turn clock doesn't leak. The session-closed
// event path clears these per session; this catches sessions a reconnect snapshot
// silently dropped (no session-closed event arrives for them).
sessions.subscribe(($sessions) => {
  const liveIds = new Set($sessions.map((s) => s.id));
  function pruneToLive<T>(current: Record<Id, T>): Record<Id, T> {
    const next = Object.fromEntries(
      Object.entries(current).filter(([id]) => liveIds.has(id)),
    ) as Record<Id, T>;
    return Object.keys(next).length === Object.keys(current).length ? current : next;
  }
  // agentStates is the authoritative per-session set, so any id in it but no
  // longer live was dropped — clear its notification turn clock too.
  for (const id of Object.keys(get(agentStates))) {
    if (!liveIds.has(id)) forgetSessionNotifications(id);
  }
  agentStates.update(pruneToLive);
  sessionAgents.update(pruneToLive);
  sessionOutputActive.update(pruneToLive);
  sessionCommands.update(pruneToLive);
});

// Keep the all-changes tab fresh when status metadata changes (file set, side,
// status, or line counts) without stealing focus. Idle polls can return a new
// object for identical content every second, so identical metadata does not
// refetch. A worktree-dirty event sets a one-shot force flag because the diff
// text can change while this metadata stays identical.
function allChangesStatusSignature(worktreeId: Id | null | undefined, files: ChangedFile[]): string {
  return [
    worktreeId ?? "",
    ...files.map(
      (file) =>
        `${file.staged ? "1" : "0"}\0${file.path}\0${file.status}\0${file.additions ?? 0}\0${file.deletions ?? 0}`,
    ),
  ].join("\n");
}

// Per-(side,path) metadata signature for one changed file. Drives the single-file
// tab live refresh: an idle status poll re-emits the same object every second, so
// matching this stored value lets identical metadata skip a refetch (mirrors the
// all-changes whole-status signature, but scoped per file so unrelated drift on
// another file never re-diffs an open tab).
function changedFileSignature(file: ChangedFile): string {
  return `${file.status}\0${file.additions ?? 0}\0${file.deletions ?? 0}`;
}
const fileTabKey = (path: string, staged: boolean | undefined) => `${staged ? "1" : "0"}\0${path}`;

let lastAllChangesSig: string | null = null;
// Last seen metadata signature per single-file tab key, so an idle poll with
// byte-identical status doesn't trigger a refresh storm. Cleared lazily below to
// the set of files currently present.
const lastSingleFileSigs = new Map<string, string>();
gitStatus.subscribe(($status) => {
  const files = $status?.files ?? [];
  const worktreeId = $status?.worktree_id;
  // Consume the one-shot force flag exactly once per status emission so both the
  // all-changes path and the single-file path below see it. A worktree-dirty edit
  // can change diff text while line counts / file set stay byte-identical, so the
  // signature gate alone would miss it (mirrors the all-changes design).
  const forceRefresh = worktreeId
    ? forcedDiffRefreshWorktrees.delete(worktreeId)
    : false;

  const hasAllChangesTab = get(diffTabs).some((tab) => tab.path === ALL_CHANGES_TAB);
  if (hasAllChangesTab) {
    const sig = allChangesStatusSignature(worktreeId, files);
    if (sig !== lastAllChangesSig || forceRefresh) {
      if (worktreeId) deleteDiffCacheForChangedFiles(worktreeId, files);
      lastAllChangesSig = sig;
      void viewAllChanges(false);
    }
  } else {
    lastAllChangesSig = null;
  }

  // Mirror the all-changes live refresh for open single-file diff tabs: when this
  // worktree's status changes (or a worktree-dirty force flag fires), evict the
  // stale cache for each open tab whose file appears in the changed set and re-run
  // viewDiff in the background (activate: false → no focus steal, no tab reorder;
  // viewDiff updates the tab text in place). Without this, an external edit (e.g. an
  // agent in the PTY) to a file with an open single-file tab would leave that tab
  // showing the pre-edit diff indefinitely. Commit tabs are immutable per-sha
  // snapshots and are never re-diffed here.
  if (!worktreeId) {
    lastSingleFileSigs.clear();
    return;
  }
  const fileSigs = new Map(files.map((file) => [fileTabKey(file.path, file.staged), changedFileSignature(file)]));
  for (const tab of get(diffTabs)) {
    if (tab.path === ALL_CHANGES_TAB || isCommitTab(tab.path)) continue;
    // Refresh a tab only when its own file's metadata signature changed OR the
    // force flag fired, so idle polls (identical signature) and unrelated drift on
    // other files never re-diff this tab.
    const tabKey = fileTabKey(tab.path, tab.staged);
    const sig = fileSigs.get(tabKey);
    const hadPrior = lastSingleFileSigs.has(tabKey);
    // First sight of this tab's file is not a change: the tab was just opened via
    // viewDiff (already fresh), so only a DIFFERING prior signature counts.
    const changed = sig !== undefined && hadPrior && sig !== lastSingleFileSigs.get(tabKey);
    if (sig !== undefined) lastSingleFileSigs.set(tabKey, sig);
    if (!changed && !forceRefresh) continue;
    invalidateDiffCacheVariants(worktreeId, tab.path, tab.staged);
    void viewDiff(tab.path, false, tab.staged);
  }
  // Forget signatures for files no longer in the status (e.g. reverted), so if the
  // same path reappears later its first status counts as a change.
  for (const key of lastSingleFileSigs.keys()) {
    if (!fileSigs.has(key)) lastSingleFileSigs.delete(key);
  }
});

// Reset the per-worktree Git view state and (re)load status whenever the target
// git worktree changes. Selecting a different worktree clears the open diff and
// commit draft; live dirty refreshes are driven by the worktree-dirty event.
let lastGitWorktreeId: Id | null = null;
gitWorktreeId.subscribe(($id) => {
  if ($id === lastGitWorktreeId) return;
  lastGitWorktreeId = $id;
  gitStatus.set(null);
  closeDiff();
  prUrl.set(null);
  prInfo.set(null);
  // Swap the History log to the new worktree (per-worktree, refetched on
  // selection). Reset the HEAD tracker so the next status doesn't mistake the new
  // worktree's HEAD for an in-place commit on the old one and skip the load.
  commitLog.set(EMPTY_COMMIT_LOG);
  commitLogHeadId = null;
  commitLogSeq += 1;
  stopGitStatusPolling();
  if ($id) {
    void loadGitStatus($id).catch((err) => error.set(toMessage(err)));
    void loadPrStatus($id);
    // Restore any daemon-owned composite chain in flight for this worktree, so
    // switching INTO a worktree mid-chain shows the exact step (and keeps
    // following its events). The query is a fast in-memory read.
    void refreshActiveJobs($id);
    startGitStatusPolling($id);
    if (get(railView) === "history") void loadCommitLog();
  }
});

// The re-diff options change the daemon request, so a change must re-fetch any
// open diff (the cache key already folds these in, so the refetch hits a fresh
// key). Skip each store's synchronous initial emission — only user toggles
// should trigger a refresh, not module load.
let diffReDiffInit = 2;
const onReDiffOptionChange = () => {
  if (diffReDiffInit > 0) {
    diffReDiffInit -= 1;
    return;
  }
  refreshOpenDiffs();
};
diffIgnoreWhitespace.subscribe(onReDiffOptionChange);
diffContextLines.subscribe(onReDiffOptionChange);

// Load the History log lazily the first time it becomes the visible rail view
// for the selected worktree (so a user who never opens HISTORY pays no `git-log`
// cost). Switching back to a worktree whose log is already loaded is a no-op.
// Skip the synchronous initial emission — startup selection drives the first load
// via the gitWorktreeId subscription above.
let railViewInit = true;
railView.subscribe(($view) => {
  if (railViewInit) {
    railViewInit = false;
    return;
  }
  if ($view !== "history") return;
  const worktreeId = get(gitWorktreeId);
  if (worktreeId && get(commitLog).worktreeId !== worktreeId) void loadCommitLog();
});
