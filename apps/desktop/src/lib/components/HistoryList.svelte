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

  // The roving (keyboard-focused) commit index is owned by RightRail — git pane
  // focus is shared with Changes, so the parent keeps the one active-row cursor
  // and the bare arrow handler. We expose the scroll container + a focus helper
  // so the parent can move/scroll the active row exactly like the file list.
  let {
    activeIndex = $bindable(-1),
  }: {
    // Index of the roving row in `commits`, or -1 when none. Two-way so the
    // parent's arrow handler and this component's click stay in sync.
    activeIndex?: number;
  } = $props();

  const log = $derived($commitLog);
  const commits = $derived(log.commits);

  let listEl = $state<HTMLElement | null>(null);
  let sentinelEl = $state<HTMLElement | null>(null);

  // The 7-char short sha shown in the meta line (libgit2 full id → display).
  function shortSha(id: string): string {
    return id.slice(0, 7);
  }

  // Coarse relative time from a unix-seconds timestamp: now / Nm / Nh / Nd, then
  // a calendar date once a week old. No external dep — the History plan asks for
  // simple coarse buckets, and no relative-time helper exists in the codebase.
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

  function openCommit(commit: CommitInfo, index: number) {
    activeIndex = index;
    openCommitTab(commit.id);
  }

  // Lazy "load more": an IntersectionObserver on a sentinel row at the list
  // bottom appends the next page when it scrolls into view. The data layer's
  // loadMoreCommits is a no-op while loading or when !hasMore, so re-fires while
  // a page is in flight are harmless. (No prior scroll-pagination pattern exists
  // in the codebase; an IO sentinel is the lightest option that doesn't poll.)
  $effect(() => {
    const sentinel = sentinelEl;
    const root = listEl;
    if (!sentinel || !root) return;
    if (!log.hasMore) return;
    const io = new IntersectionObserver(
      (entries) => {
        if (entries.some((e) => e.isIntersecting)) void loadMoreCommits();
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
    void tick().then(() => {
      const row = listEl?.querySelector<HTMLElement>(`.crow[data-index="${activeIndex}"]`);
      row?.focus();
      row?.scrollIntoView({ block: "nearest" });
    });
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
      {#each commits as commit, i (commit.id)}
        <li>
          <button
            class="crow"
            data-index={i}
            data-sha={commit.id}
            class:active={$diffActive && $activeDiffPath === commitTabPath(commit.id)}
            class:roving={activeIndex === i}
            title={commitTitle(commit)}
            onclick={() => openCommit(commit, i)}
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
