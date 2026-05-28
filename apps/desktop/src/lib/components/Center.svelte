<script lang="ts">
  // Center column (mockup .center): connection banner, then either an empty
  // prompt or the session tabs + live terminals for the active parent. Every
  // open session — across ALL parents, not just the active one — keeps a live
  // (warm) xterm: we render the keyed list off `$sessions` and toggle
  // VISIBILITY rather than mounting/unmounting per tab, diff, or worktree
  // switch. That lets each terminal preserve its scroll position, buffer,
  // and (crucially) its PTY-aligned grid: remounting a Terminal would create
  // a fresh xterm that re-parses the byte ring at whatever size the new host
  // happened to measure, and any drift from the PTY's actual cols/rows
  // displaces wrapped lines and cursor-addressed TUI output. Memory cost is
  // bounded by the session count (each xterm holds ~5000 lines of scrollback)
  // and only one terminal at a time runs the WebGL renderer (the active one),
  // so live GPU contexts stay well under the browser cap.
  import {
    activeSessionId,
    connection,
    diffActive,
    diffPath,
    error,
    reconnect,
    selectedParent,
    selectedProject,
    sessions,
    visibleSessions,
    worktrees,
  } from "../daemon";
  import { sessionBelongsTo } from "../types";
  import SessionTabs from "./SessionTabs.svelte";
  import Terminal from "./Terminal.svelte";
  import DiffTab from "./DiffTab.svelte";

  const parentLabel = $derived(
    $selectedParent?.kind === "worktree"
      ? ($worktrees.find((w) => w.id === $selectedParent?.id)?.branch ??
          $selectedProject?.name)
      : $selectedProject?.name,
  );
</script>

<main class="center">
  {#if $connection === "offline"}
    <div class="banner offline">
      {$error ?? "Daemon offline — sessions are detached."}
      <button class="pill" onclick={() => void reconnect()}>Reconnect</button>
    </div>
  {:else if $connection === "connecting"}
    <div class="banner connecting">Starting daemon…</div>
  {/if}

  {#if $selectedParent}
    <SessionTabs parent={$selectedParent} />
  {/if}

  <!-- The view stays mounted regardless of which empty/overlay state is up,
       so the terminals inside survive worktree/project switches. Each slot
       is keyed by session id so its Terminal instance is stable for the
       session's lifetime — only torn down when the session itself closes.
       A slot is visible only when its session belongs to the active parent
       AND is the active session AND the diff isn't covering the view. -->
  <div class="view">
    {#each $sessions as session (session.id)}
      {@const inActiveParent = sessionBelongsTo(session, $selectedParent)}
      {@const visible = inActiveParent && session.id === $activeSessionId && !$diffActive}
      <div class="slot" class:hidden={!visible}>
        <Terminal {session} active={visible} />
      </div>
    {/each}

    {#if !$selectedProject}
      <div class="empty">
        <h3>No project selected</h3>
        <p>Add a local repo or folder, then pick a worktree to open sessions.</p>
      </div>
    {:else if !$selectedParent}
      <div class="empty">
        <h3>Choose a worktree</h3>
        <p>
          Select a worktree under <span class="mono">{$selectedProject.name}</span> to open a
          shell or launch an agent.
        </p>
      </div>
    {:else if $diffActive && $diffPath}
      <!-- The diff OVERLAYS the (now hidden) terminals rather than replacing
           them; closing it reveals the active terminal with scroll + buffer
           intact, since it was never destroyed. -->
      <DiffTab />
    {:else if $visibleSessions.length === 0}
      <div class="empty">
        <h3>No live session</h3>
        <p>
          Open a shell or launch an agent in
          <span class="mono">{parentLabel}</span>. Output survives quitting Hitch.
        </p>
      </div>
    {/if}
  </div>
</main>

<style>
  .center {
    display: flex;
    flex-direction: column;
    min-width: 0;
    min-height: 0;
    height: 100%;
    overflow: hidden;
    background: var(--bg-1);
  }
  .view {
    flex: 1;
    min-height: 0;
    position: relative;
    overflow: hidden;
  }
  /* Each warm terminal fills the view; the active one shows, the rest are
     display:none (kept mounted, never measured — the Terminal's zero-size
     guard skips fitting while hidden and fit-on-activate catches it up). */
  .slot {
    position: absolute;
    inset: 0;
  }
  .slot.hidden {
    display: none;
  }

  .empty {
    flex: 1;
    height: 100%;
    display: grid;
    /* `safe` keeps a tall empty-state pinned to the top instead of overflowing
       upward into the nav when the column is narrow. */
    place-content: safe center;
    justify-items: center;
    text-align: center;
    gap: 7px;
    padding: 24px;
  }
  .empty h3 {
    font-size: 13px;
    font-weight: 560;
    color: var(--tx-hi);
  }
  .empty p {
    font-size: 12px;
    color: var(--tx-lo);
    max-width: 300px;
    line-height: 1.55;
  }
  .empty .mono {
    font-family: var(--mono);
    color: var(--tx-md);
  }

  .banner {
    flex: none;
    display: flex;
    align-items: center;
    gap: 9px;
    padding: 9px 14px;
    font-size: 12px;
  }
  .banner.offline {
    background: oklch(58% 0.14 25 / 0.14);
    border-bottom: 1px solid oklch(58% 0.14 25 / 0.3);
    color: oklch(86% 0.06 25);
  }
  .banner.connecting {
    background: oklch(60% 0.08 265 / 0.12);
    border-bottom: 1px solid var(--line);
    color: var(--tx-md);
  }
  .banner .pill {
    margin-left: auto;
    padding: 4px 10px;
    font-size: 11px;
    border-radius: var(--radius);
    background: transparent;
    cursor: pointer;
    font-family: var(--ui);
    border: 1px solid oklch(58% 0.14 25 / 0.45);
    color: oklch(88% 0.05 25);
  }
</style>
