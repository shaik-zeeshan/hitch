<script lang="ts">
  // Center column (mockup .center): connection banner, then either an empty
  // prompt or the session tabs + live terminals for the active parent. Every
  // session of the active parent keeps a live (warm) xterm: we render them all
  // keyed by id and toggle VISIBILITY rather than mounting/unmounting per tab
  // or diff switch, so switching tabs or opening/closing the diff is instant
  // and preserves each terminal's scroll position and buffer. The keyed set is
  // scoped to the active parent (`visibleSessions`), so switching parents
  // naturally tears down the old parent's terminals and mounts the new ones —
  // bounding how many xterm instances are live at once.
  import {
    activeSessionId,
    connection,
    diffActive,
    diffPath,
    error,
    reconnect,
    selectedParent,
    selectedProject,
    visibleSessions,
    worktrees,
  } from "../daemon";
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
  {:else}
    <SessionTabs parent={$selectedParent} />
    <div class="view">
      <!-- Keep every session of the active parent mounted. Each slot is keyed
           by session id so its Terminal instance is stable for the session's
           lifetime (mounted once, reused) and is only torn down when the
           session closes or the parent changes. Visibility — not mount state —
           tracks which terminal is shown: a slot is visible only when it's the
           active session AND the diff isn't covering the view. -->
      {#each $visibleSessions as session (session.id)}
        {@const visible = session.id === $activeSessionId && !$diffActive}
        <div class="slot" class:hidden={!visible}>
          <Terminal {session} active={visible} />
        </div>
      {/each}

      {#if $diffActive && $diffPath}
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
  {/if}
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
