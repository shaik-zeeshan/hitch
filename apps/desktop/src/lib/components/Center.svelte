<script lang="ts">
  // Center column (mockup .center): the session tabs + live terminals for the
  // active parent, or an empty state — no project, no worktree, no live session,
  // or the daemon being unavailable (its status detail + restart live in the top
  // nav; here we just stand in for the terminal it can't provide). Every
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
    ALL_CHANGES_TAB,
    connection,
    activeDiffPath,
    diffActive,
    commitShaFromTab,
    error,
    reconnect,
    selectedParent,
    selectedProject,
    sessions,
    visibleSessions,
    worktrees,
  } from "../daemon";
  import { sessionBelongsTo } from "../types";
  import { isDark } from "../theme";
  import {
    terminalSurfaceOverride,
    terminalThemeDark,
    terminalThemeLight,
  } from "../terminal-themes";
  import SessionTabs from "./SessionTabs.svelte";
  import Terminal from "./Terminal.svelte";
  import DiffTab from "./DiffTab.svelte";
  import DiffAllTab from "./DiffAllTab.svelte";
  import CommitTab from "./CommitTab.svelte";

  // Re-theme every --term-* consumer in the center column at one bind point: the
  // tab strip's active tab, the terminal panels' insets/overlays, and the diff
  // view all read --term-bg2/--term-fg/--term-dim/--term-line, and .center is the
  // tightest box that scopes them as a group (the empty states use only paper/ink
  // vars, so they're unaffected). Without this the active tab would keep the
  // built-in surface color while the terminal below shows the theme background,
  // leaving a visible seam. Re-derives on every app-mode flip and either selection
  // change (same axes as Terminal's palette $effect): $isDark picks the mode, the
  // two selection stores pick the id; "" for the built-in theme.
  const surfaceOverride = $derived(
    terminalSurfaceOverride($isDark ? $terminalThemeDark : $terminalThemeLight),
  );

  const parentLabel = $derived(
    $selectedParent?.kind === "worktree"
      ? ($worktrees.find((w) => w.id === $selectedParent?.id)?.branch ??
          $selectedProject?.name)
      : $selectedProject?.name,
  );
</script>

<main class="center" style={surfaceOverride}>
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
      {@const visible =
        $connection === "ready" &&
        inActiveParent &&
        session.id === $activeSessionId &&
        !$diffActive}
      <div class="slot" class:hidden={!visible}>
        <Terminal {session} active={visible} />
      </div>
    {/each}

    {#if $connection !== "ready"}
      <!-- The daemon owns every PTY, so without it there's nothing live to show.
           Replace the terminal with a state that explains the situation and
           offers a way back rather than leaving a stale, detached buffer up. -->
      <div class="empty">
        {#if $connection === "connecting"}
          <h3>Starting daemon…</h3>
          <p>Connecting to the Hitch daemon that owns your sessions.</p>
        {:else}
          <h3>Daemon offline</h3>
          <p>
            {$error ??
              "The Hitch daemon isn't running. Sessions are detached, but their output is preserved and will reattach once it's back."}
          </p>
          <button class="action" onclick={() => void reconnect()}>Reconnect</button>
        {/if}
      </div>
    {:else if !$selectedProject}
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
    {:else if $diffActive && $activeDiffPath === ALL_CHANGES_TAB}
      <!-- The all-changes view: every changed file in one scroll, as collapsible
           sections. Same overlay behavior as a single-file diff. -->
      <DiffAllTab />
    {:else if $diffActive && $activeDiffPath && commitShaFromTab($activeDiffPath) !== null}
      <!-- A Commit Tab: one immutable commit's metadata header + collapsible
           per-file sections, keyed by sha. Keyed on the sha so switching between
           open commit tabs remounts with the right commit (the per-sha diff
           cache means a remount never refetches an already-loaded commit). -->
      {#key $activeDiffPath}
        <CommitTab sha={commitShaFromTab($activeDiffPath)!} />
      {/key}
    {:else if $diffActive && $activeDiffPath}
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
    background: var(--paper-3);
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
    font-family: var(--ui);
    font-size: var(--r1);
    font-weight: 600;
    color: var(--ink-1);
  }
  .empty p {
    font-family: var(--ui);
    font-size: var(--r0);
    color: var(--ink-2);
    max-width: 300px;
    line-height: 1.55;
  }
  .empty .mono {
    font-family: var(--mono);
    color: var(--ink-1);
  }
  /* Quiet letterpress button: hairline, square, paper fill. */
  .empty .action {
    margin-top: 5px;
    padding: 6px 12px;
    font-family: var(--ui);
    font-size: var(--r0);
    border-radius: 0;
    border: 1px solid var(--line);
    background: var(--paper-2);
    color: var(--ink-1);
    cursor: pointer;
    transition:
      border-color 0.15s ease-out,
      color 0.15s ease-out;
  }
  .empty .action:hover {
    border-color: var(--ink-3);
    color: var(--ink-0);
  }
</style>
