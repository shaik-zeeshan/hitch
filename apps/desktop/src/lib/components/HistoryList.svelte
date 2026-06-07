<script lang="ts">
  // Right rail — HISTORY view (Paper Terminal shell). The selected worktree's
  // HEAD commit log as two-line rows: summary (strong ink) over
  // `▍ sha · relative-time · author` (dimmed; the ▍ iris bar appears ONLY when
  // the commit is ahead of base). Merge commits carry a small `merge` badge.
  // Clicking a row opens its Commit Tab. Scrolling to the bottom appends the
  // next page (loadMoreCommits). Roving focus (↑/↓ clamped, Enter opens) is
  // driven from RightRail through the bound props below, mirroring the Changes
  // file-list roving — there is no second key handler in this component.
  import { tick } from "svelte";
  import GitMerge from "~icons/lucide/git-merge";
  import {
    activeDiffPath,
    commitLog,
    diffActive,
    gitWorktreeId,
    loadMoreCommits,
    openCommitTab,
    commitTabPath,
  } from "../daemon";
  import type { CommitInfo } from "../types";
  import { focusWithoutScroll } from "../focusWithoutScroll";

  // The roving (keyboard-focused) commit is owned by RightRail — git pane focus
  // is shared with Changes, so the parent keeps the one active-row cursor and the
  // bare arrow handler. The cursor is tracked by commit SHA (not array index) so
  // it survives a pagination append (which keeps earlier rows but is index-fragile
  // when a row's batch matters) and a reset/refetch that reorders the array. We
  // expose the scroll container + a focus helper so the parent can move/scroll the
  // active row exactly like the file list. `openCommit` is exported so the parent's
  // keyboard Enter and this component's click go through ONE code path: both open
  // the tab AND set the same id-based selection, so the roving/active visuals land
  // on the clicked/Entered row regardless of which page it loaded in.
  let {
    activeSha = $bindable(null),
  }: {
    // SHA of the roving row, or null when none. Two-way so the parent's arrow
    // handler and this component's click stay in sync.
    activeSha?: string | null;
  } = $props();

  const log = $derived($commitLog);
  const commits = $derived(log.commits);

  let listEl = $state<HTMLElement | null>(null);
  let sentinelEl = $state<HTMLElement | null>(null);

  // Keep the viewport anchored when a HEAD-move refresh PREPENDS new commits at
  // the top. Without this, inserting rows above the scroll position slides every
  // visible row down by the inserted height while scrollTop stays put — so under
  // a churning HEAD (an agent committing in the selected PTY) the list appears to
  // drift/"snap", and a row the user is reaching for moves out from under the
  // pointer between mousedown and mouseup (the browser then synthesizes no click).
  // `$effect.pre` records scrollHeight BEFORE the prepended rows lay out; the post
  // effect adds the height delta to scrollTop so the same commit stays under the
  // cursor. Both key off `log.tick` (bumped once per prepend) so a normal append
  // or reset — which must NOT shift scroll — never triggers compensation. We skip
  // when scrolled to the very top (scrollTop ~0): there the user wants to see the
  // new commits arrive, matching how a log viewer behaves at HEAD.
  let anchorTick = -1;
  let anchorScrollHeight = 0;
  let anchorAtTop = true;
  $effect.pre(() => {
    const tick = log.tick;
    if (tick === anchorTick || log.prependedCount === 0 || !listEl) return;
    anchorTick = tick;
    anchorScrollHeight = listEl.scrollHeight;
    anchorAtTop = listEl.scrollTop <= 1;
  });
  let appliedTick = -1;
  $effect(() => {
    const tick = log.tick;
    // Touch commits so this re-runs after the prepended rows are in the DOM.
    void commits.length;
    if (tick === appliedTick || tick !== anchorTick || !listEl || anchorAtTop) return;
    appliedTick = tick;
    const delta = listEl.scrollHeight - anchorScrollHeight;
    if (delta > 0) listEl.scrollTop += delta;
  });

  // The 7-char short sha shown in the meta line (libgit2 full id → display).
  function shortSha(id: string): string {
    return id.slice(0, 7);
  }

  // Coarse relative time from a unix-seconds timestamp: now / Nm / Nh / Nd, then
  // a calendar date once a week old. No external dep — the History plan asks for
  // simple coarse buckets, and no relative-time helper exists in the codebase.
  //
  // Not memoized: it's a handful of integer comparisons, so it's recomputed every
  // render. Labels therefore advance only when a row re-renders — a pagination
  // append, a HEAD-move refresh, or selection/roving change — not on a wall-clock
  // timer (the component subscribes to no per-second store, and adding one for a
  // cosmetic label isn't worth it). An old cache keyed on the immutable timestamp
  // would have frozen each label forever, so a fresh recompute is the correct call.
  function relativeTime(unixSeconds: number): string {
    const deltaMs = Date.now() - unixSeconds * 1000;
    const sec = Math.floor(deltaMs / 1000);
    if (sec < 60) return "now";
    const min = Math.floor(sec / 60);
    if (min < 60) return `${min}m`;
    const hr = Math.floor(min / 60);
    if (hr < 24) return `${hr}h`;
    const day = Math.floor(hr / 24);
    if (day < 7) return `${day}d`;
    return new Date(unixSeconds * 1000).toLocaleDateString(undefined, {
      month: "short",
      day: "numeric",
    });
  }

  function commitTitle(commit: CommitInfo): string {
    return commit.summary ?? "(no commit message)";
  }

  // The single open path for both a row click and the parent's keyboard Enter:
  // set the id-based roving cursor AND open the commit tab. Exported so RightRail's
  // openActiveCommit() reuses it verbatim — neither path can open a tab without
  // also syncing the selection, so the roving visual always follows the opened row
  // (the blue `.active` stick is id-derived from $activeDiffPath separately and is
  // already correct for any row regardless of which page loaded it).
  export function openCommit(commit: CommitInfo) {
    activeSha = commit.id;
    openCommitTab(commit.id);
  }

  // Lazy "load more": an IntersectionObserver on a sentinel row at the list
  // bottom appends the next page when it scrolls into view. The observer depends
  // ONLY on the sentinel + scroll-root elements — NOT on `hasMore`/`loading`, so a
  // page landing (which flips both) does not tear down and rebuild the observer on
  // every append. Freshness is read live inside the callback instead: we bail if no
  // more pages or a page is already in flight, so the sentinel firing repeatedly
  // while a load is pending is a cheap no-op rather than a duplicate request.
  // (No prior scroll-pagination pattern exists in the codebase; an IO sentinel is
  // the lightest option that doesn't poll.)
  $effect(() => {
    const sentinel = sentinelEl;
    const root = listEl;
    if (!sentinel || !root) return;
    const io = new IntersectionObserver(
      (entries) => {
        if (!entries.some((e) => e.isIntersecting)) return;
        // Read freshness live so a stale closure can't request past the end or
        // double-fire while a page is in flight. loadMoreCommits is itself a
        // no-op while loading, but gating here avoids the wasted call entirely.
        if (!log.hasMore || log.loading) return;
        void loadMoreCommits();
      },
      { root, rootMargin: "120px" },
    );
    io.observe(sentinel);
    return () => io.disconnect();
  });

  // Keep the roving row scrolled into view when the parent moves it. The DOM
  // focus move itself is the parent's job (it owns the cursor); this only
  // mirrors the file list's scrollIntoView so a long log stays usable.
  export function scrollActiveIntoView() {
    if (activeSha === null) return;
    const sha = activeSha;
    void tick().then(() => {
      const row = listEl?.querySelector<HTMLElement>(`.crow[data-sha="${cssEscape(sha)}"]`);
      row?.focus();
      row?.scrollIntoView({ block: "nearest" });
    });
  }

  // Minimal CSS.escape fallback for the sha attribute selector (a full git id is
  // hex + safe, but guard the same way RightRail does for the test/SSR env).
  function cssEscape(value: string): string {
    return typeof CSS !== "undefined" && CSS.escape ? CSS.escape(value) : value;
  }
</script>

<div class="history" bind:this={listEl}>
  {#if !$gitWorktreeId}
    <div class="empty"><p>Select a git worktree to see its history.</p></div>
  {:else if commits.length === 0}
    {#if log.loading}
      <div class="empty"><p>Loading history…</p></div>
    {:else}
      <div class="empty"><p>No commits yet.</p></div>
    {/if}
  {:else}
    <ul class="clist">
      {#each commits as commit (commit.id)}
        <li>
          <button
            class="crow"
            data-sha={commit.id}
            class:active={$diffActive && $activeDiffPath === commitTabPath(commit.id)}
            class:roving={activeSha === commit.id}
            title={commitTitle(commit)}
            onpointerdown={focusWithoutScroll}
            onclick={() => openCommit(commit)}
          >
            <span class="summary">{commitTitle(commit)}</span>
            <span class="meta">
              <span class="bar" class:ahead={commit.ahead_of_base} aria-hidden="true">▍</span>
              <span class="sha">{shortSha(commit.id)}</span>
              <span class="sep" aria-hidden="true">·</span>
              <span class="time">{relativeTime(commit.time)}</span>
              {#if commit.author}
                <span class="sep" aria-hidden="true">·</span>
                <span class="author">{commit.author}</span>
              {/if}
              {#if commit.is_merge}
                <span class="badge"><GitMerge class="badgeic icon" />merge</span>
              {/if}
            </span>
          </button>
        </li>
      {/each}
    </ul>

    <!-- Load-more sentinel + a subtle loading row while a page is in flight. -->
    {#if log.hasMore}
      <div class="sentinel" bind:this={sentinelEl}></div>
    {/if}
    {#if log.loading && commits.length > 0}
      <div class="loading-row">Loading more…</div>
    {/if}
  {/if}
</div>

<style>
  .history {
    flex: 1;
    overflow: auto;
    min-height: 0;
    padding: 6px 10px 12px;
  }

  .empty {
    padding: 38px 20px;
    text-align: center;
  }
  .empty p {
    font-size: var(--r1);
    color: var(--ink-3);
    line-height: 1.55;
  }

  .clist {
    list-style: none;
    margin: 0;
    padding: 0;
  }

  /* Two-line commit row. Reuses the file-row selection language verbatim: the
     roving (keyboard) row and the open-tab (.active) row both get the sunken
     paper-3 fill + inset hairline — no new focus treatment. */
  .crow {
    display: flex;
    flex-direction: column;
    gap: 3px;
    width: 100%;
    text-align: left;
    padding: 6px 8px;
    border: 0;
    border-radius: 0;
    background: transparent;
    cursor: pointer;
    transition: background 0.15s ease-out;
  }
  .crow:hover {
    background: var(--paper-3);
  }
  .crow.active,
  .crow.roving {
    background: var(--paper-3);
    box-shadow: inset 0 0 0 1px var(--line);
  }
  .crow:focus-visible {
    outline: none;
    background: var(--paper-3);
    box-shadow: inset 0 0 0 1px var(--line);
  }

  /* Line 1: commit summary — strong ink, single line, truncated. */
  .crow .summary {
    font-family: var(--mono);
    font-size: var(--r1);
    font-weight: 500;
    color: var(--ink-0);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  /* Line 2: ▍ sha · relative-time · author — dimmed meta, mono tabular. */
  .crow .meta {
    display: flex;
    align-items: center;
    gap: 5px;
    min-width: 0;
    font-family: var(--mono);
    font-size: 0.625rem;
    color: var(--ink-2);
    font-variant-numeric: tabular-nums;
    overflow: hidden;
  }
  /* Branch-work marker: an iris bar shown ONLY for commits ahead of base.
     Otherwise the glyph is invisible but keeps the meta line's left edge aligned
     across rows (no jump between ahead / not-ahead rows). */
  .crow .meta .bar {
    flex: none;
    color: transparent;
    margin-right: -2px;
  }
  .crow .meta .bar.ahead {
    color: var(--iris);
  }
  .crow .meta .sha {
    flex: none;
    font-weight: 600;
    color: var(--ink-1);
  }
  .crow .meta .sep {
    flex: none;
    color: var(--ink-3);
  }
  .crow .meta .time {
    flex: none;
  }
  .crow .meta .author {
    flex: 0 1 auto;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  /* Merge badge — small mono pill, hairline, quiet. */
  .crow .meta .badge {
    flex: none;
    margin-left: 2px;
    display: inline-flex;
    align-items: center;
    gap: 3px;
    padding: 1px 5px 1px 4px;
    font-size: 0.5625rem;
    font-weight: 600;
    letter-spacing: 0.02em;
    color: var(--ink-2);
    background: var(--paper-2);
    border: 1px solid var(--line);
    border-radius: 0;
  }
  .crow .meta .badge :global(.badgeic) {
    width: 9px;
    height: 9px;
    flex: 0 0 9px;
    color: var(--ink-3);
  }

  .sentinel {
    height: 1px;
  }
  .loading-row {
    padding: 8px;
    text-align: center;
    font-family: var(--mono);
    font-size: 0.625rem;
    color: var(--ink-3);
  }
</style>
