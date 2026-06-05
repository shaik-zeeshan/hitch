<script lang="ts">
  // Session tabs (Paper Terminal .tabs). One tab per session in the active
  // parent, then — when a changed file is open — the diff tab as a peer. Each
  // tab leads with a harness mark from the announced agent identity (Claude /
  // Codex / shell), titles active agent sessions with their product name, and
  // shows a 6px state dot ONLY for act states (needs-approval / error). The
  // active tab is the terminal surface itself, bridging into the panel below.
  // × closes; the trailing + opens a dropdown to spawn Claude / Codex / a plain
  // shell in the active parent.
  import { ContextMenu, DropdownMenu } from "bits-ui";
  import Plus from "~icons/lucide/plus";
  import X from "~icons/lucide/x";
  import Claude from "~icons/hitch/claude";
  import Codex from "~icons/hitch/codex";
  import Shell from "~icons/hitch/shell";
  import {
    activeSession,
    activeSessionId,
    displaySessionStates,
    closeDiff,
    closeSession,
    diffActive,
    diffPath,
    openSession,
    sessionAgents,
    sessionCommands,
    visibleSessions,
  } from "../daemon";
  import { needsAction, type Session, type SessionParent } from "../types";
  import { sessionTabKind, sessionTabTitle } from "../sessionDisplay";
  let { parent }: { parent: SessionParent } = $props();

  const diffName = $derived($diffPath?.split("/").pop() ?? "diff");

  function select(session: Session) {
    diffActive.set(false);
    activeSessionId.set(session.id);
  }
</script>

<div class="tabs" role="tablist">
  {#each $visibleSessions as session (session.id)}
    {@const state = $displaySessionStates[session.id]}
    {@const agent = $sessionAgents[session.id]}
    {@const command = $sessionCommands[session.id]}
    {@const title = sessionTabTitle(agent, session.name, command)}
    {@const kind = sessionTabKind(agent)}
    {@const active = !$diffActive && session.id === $activeSession?.id}
    <ContextMenu.Root>
      <ContextMenu.Trigger>
        {#snippet child({ props })}
          <button
            {...props}
            class="tab"
            class:active
            role="tab"
            aria-selected={active}
            {title}
            onclick={() => select(session)}
          >
            {#if kind === "claude"}
              <Claude class="icon tabmark claude" />
            {:else if kind === "codex"}
              <Codex class="icon tabmark codex" />
            {:else}
              <Shell class="icon tabmark shell" />
            {/if}

            <span class="name">{title}</span>

            {#if needsAction(state)}
              <!-- Act state (needs-approval / error): one oxide dot, "act here".
                   Never shown for working/waiting/idle. -->
              <span class="needdot" aria-label="needs attention"></span>
            {/if}

            <span
              class="closer"
              role="button"
              tabindex="-1"
              aria-label="Close session"
              title="Close session"
              onclick={(e) => {
                e.stopPropagation();
                void closeSession(session);
              }}
              onkeydown={() => {}}
            >
              <X class="icon" />
            </span>
          </button>
        {/snippet}
      </ContextMenu.Trigger>
      <ContextMenu.Portal>
        <ContextMenu.Content class="menu">
          <ContextMenu.Item class="mi danger" onSelect={() => void closeSession(session)}>
            <X class="mi-ico" />
            Close session
          </ContextMenu.Item>
        </ContextMenu.Content>
      </ContextMenu.Portal>
    </ContextMenu.Root>
  {/each}

  {#if $diffPath}
    <button
      class="tab"
      class:active={$diffActive}
      role="tab"
      aria-selected={$diffActive}
      title={$diffPath}
      onclick={() => diffActive.set(true)}
    >
      <span class="tabmark glyph" aria-hidden="true">±</span>
      <span class="name">{diffName}</span>
      <span
        class="closer"
        role="button"
        tabindex="-1"
        aria-label="Close diff"
        title="Close diff"
        onclick={(e) => {
          e.stopPropagation();
          closeDiff();
        }}
        onkeydown={() => {}}
      >
        <X class="icon" />
      </span>
    </button>
  {/if}

  <DropdownMenu.Root>
    <DropdownMenu.Trigger>
      {#snippet child({ props })}
        <button {...props} class="newtab" title="New session" aria-label="New session">
          <Plus class="icon" />
        </button>
      {/snippet}
    </DropdownMenu.Trigger>
    <DropdownMenu.Portal>
      <DropdownMenu.Content class="menu" align="start" sideOffset={6}>
        <DropdownMenu.Item class="mi" onSelect={() => void openSession(parent, "claude", ["claude"])}>
          <Claude class="mi-ico claude" />
          Claude
        </DropdownMenu.Item>
        <DropdownMenu.Item class="mi" onSelect={() => void openSession(parent, "codex", ["codex"])}>
          <Codex class="mi-ico" />
          Codex
        </DropdownMenu.Item>
        <DropdownMenu.Separator class="m-sep" />
        <DropdownMenu.Item class="mi" onSelect={() => void openSession(parent, "shell", null)}>
          <Shell class="mi-ico" />
          Shell
        </DropdownMenu.Item>
      </DropdownMenu.Content>
    </DropdownMenu.Portal>
  </DropdownMenu.Root>
</div>

<style>
  .tabs {
    flex: 0 0 38px;
    height: 38px;
    display: flex;
    align-items: stretch;
    gap: 0;
    /* Zero strip padding: the first tab hugs the left column divider. */
    padding: 0;
    background: var(--paper-3);
    /* The terminal's own hairline — the active tab's ink bridges across it. */
    border-bottom: 1px solid var(--term-line);
    position: relative;
    z-index: 2;
    overflow-x: auto;
    flex-wrap: nowrap;
    scrollbar-width: thin;
  }
  .tab {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    font-family: var(--mono);
    font-size: var(--r1);
    color: var(--ink-2);
    padding: 0 14px;
    border: 1px solid transparent;
    border-bottom: none;
    border-radius: 0;
    position: relative;
    cursor: pointer;
    background: transparent;
    white-space: nowrap;
    flex: none;
    transition: color 0.15s ease-out;
  }
  .tab:hover {
    color: var(--ink-1);
  }
  .tab.active {
    /* The active tab IS the terminal surface: same ink fill running from the
       strip's top hairline straight down into the panel. */
    background: var(--term-bg2);
    color: var(--term-fg);
    font-weight: 600;
    border-color: var(--term-line);
    border-top-color: transparent;
  }
  /* Bridge over the strip's bottom hairline so the ink runs unbroken into the
     panel below the tab. */
  .tab.active::after {
    content: "";
    position: absolute;
    left: 0;
    right: 0;
    bottom: -1px;
    height: 2px;
    background: var(--term-bg2);
    z-index: 3;
  }
  .tab:focus-visible {
    outline: 1px solid var(--iris-ink);
    outline-offset: -2px;
  }
  .tab .name {
    font-family: var(--mono);
  }

  .tabmark {
    width: 14px;
    height: 14px;
    flex: none;
    color: var(--ink-2);
  }
  /* Claude coral reads against both the paper strip and the active dark ink. */
  .tabmark.claude {
    color: var(--mark-claude);
  }
  /* :global on the descendant marks: the classes live on the icon child
     component, which Svelte's scoped analyzer can't see through. */
  .tab.active :global(.tabmark.shell),
  .tab.active :global(.tabmark.codex) {
    color: var(--term-dim);
  }
  /* Diff tab uses a ± glyph instead of a harness mark. */
  .tabmark.glyph {
    display: inline-grid;
    place-items: center;
    font-weight: 600;
    font-size: 0.875rem;
    line-height: 1;
  }

  /* Act-state dot: 6px oxide circle, ringed in the terminal ink so it reads on
     the active tab. Shown iff the session is in an act state. */
  .needdot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    flex: none;
    background: var(--st-need);
    box-shadow: 0 0 0 3px var(--term-bg2);
  }

  .closer {
    display: inline-grid;
    place-items: center;
    width: 16px;
    height: 16px;
    color: var(--ink-3);
    cursor: pointer;
    border-radius: 0;
    /* Kept in the DOM (opacity, not display) so it stays focus-reachable. */
    opacity: 0;
    transition: opacity 0.15s ease-out, color 0.15s ease-out;
  }
  .closer :global(svg) {
    width: 0.7rem;
    height: 0.7rem;
  }
  .tab:hover .closer,
  .tab.active .closer,
  .closer:focus-visible {
    opacity: 1;
  }
  .tab.active .closer {
    color: var(--term-dim);
  }
  .closer:hover {
    color: var(--term-fg);
  }
  .tab:not(.active) .closer:hover {
    color: var(--ink-0);
  }

  .newtab {
    display: grid;
    place-items: center;
    width: 32px;
    align-self: stretch;
    color: var(--ink-3);
    cursor: pointer;
    background: transparent;
    border: 0;
    flex: none;
    transition: color 0.15s ease-out;
  }
  .newtab :global(svg) {
    width: 15px;
    height: 15px;
  }
  .newtab:hover,
  .newtab[data-state="open"] {
    color: var(--ink-1);
  }
  .newtab:focus-visible {
    outline: 1px solid var(--iris-ink);
    outline-offset: -2px;
  }

  /* Claude coral carries into the new-session menu mark too. */
  .mi-ico.claude {
    color: var(--mark-claude);
  }
</style>
