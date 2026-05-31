<script lang="ts">
  // Session tabs (mockup .tabs). One tab per session in the active parent, then
  // — when a changed file is open — a divider and the diff tab as a peer.
  // Each tab leads with an icon from the active foreground command when present
  // (Claude / Codex / shell), titles active agent sessions with their product
  // name, and trails the hook-reported state word when present
  // (the runtag). × closes, and the trailing + opens a dropdown to spawn
  // Claude / Codex / a plain shell in the active parent.
  import { ContextMenu, DropdownMenu } from "bits-ui";
  import {
    activeSession,
    activeSessionId,
    visibleAgentStates,
    closeDiff,
    closeSession,
    diffActive,
    diffPath,
    openSession,
    sessionCommands,
    visibleSessions,
  } from "../daemon";
  import { AGENT_LABEL, type Session, type SessionParent } from "../types";
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
    {@const state = $visibleAgentStates[session.id]}
    {@const command = $sessionCommands[session.id]}
    {@const title = sessionTabTitle(session.name, command)}
    {@const kind = sessionTabKind(session.name, command)}
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
              <!-- Anthropic / Claude sunburst -->
              <svg class="kind claude" viewBox="0 0 16 16" fill="none" stroke="currentColor"
                stroke-width="1.5" stroke-linecap="round"
                ><line x1="8" y1="2.4" x2="8" y2="13.6" /><line x1="2.4" y1="8" x2="13.6" y2="8" /><line
                  x1="4.05"
                  y1="4.05"
                  x2="11.95"
                  y2="11.95"
                /><line x1="11.95" y1="4.05" x2="4.05" y2="11.95" /></svg
              >
            {:else if kind === "codex"}
              <!-- OpenAI / Codex logomark -->
              <svg class="kind" viewBox="0 0 24 24" fill="currentColor"
                ><path
                  d="M22.2819 9.8211a5.9847 5.9847 0 0 0-.5157-4.9108 6.0462 6.0462 0 0 0-6.5098-2.9A6.0651 6.0651 0 0 0 4.9807 4.1818a5.9847 5.9847 0 0 0-3.9977 2.9 6.0462 6.0462 0 0 0 .7427 7.0966 5.98 5.98 0 0 0 .511 4.9107 6.051 6.051 0 0 0 6.5146 2.9001A5.9847 5.9847 0 0 0 13.2599 24a6.0557 6.0557 0 0 0 5.7718-4.2058 5.9894 5.9894 0 0 0 3.9977-2.9001 6.0557 6.0557 0 0 0-.7475-7.0729zm-9.022 12.6081a4.4755 4.4755 0 0 1-2.8764-1.0408l.1419-.0804 4.7783-2.7582a.7948.7948 0 0 0 .3927-.6813v-6.7369l2.02 1.1686a.071.071 0 0 1 .038.052v5.5826a4.504 4.504 0 0 1-4.5165 4.4924zm-9.6607-4.1254a4.4708 4.4708 0 0 1-.5346-3.0137l.1419.0852 4.783 2.7582a.7712.7712 0 0 0 .7806 0l5.8428-3.3685v2.3324a.0804.0804 0 0 1-.0332.0615L9.74 19.9502a4.4992 4.4992 0 0 1-6.1408-1.6464zM2.3408 7.8956a4.485 4.485 0 0 1 2.3655-1.9728V11.6a.7664.7664 0 0 0 .3879.6765l5.8144 3.3543-2.0201 1.1685a.0757.0757 0 0 1-.071 0l-4.8303-2.7865A4.504 4.504 0 0 1 2.3408 7.872zm16.5963 3.8558L13.1038 8.364 15.1192 7.2a.0757.0757 0 0 1 .071 0l4.8303 2.7913a4.4944 4.4944 0 0 1-.6765 8.1042v-5.6772a.79.79 0 0 0-.407-.667zm2.0107-3.0231l-.142-.0852-4.7735-2.7818a.7759.7759 0 0 0-.7854 0L9.409 9.2297V6.8974a.0662.0662 0 0 1 .0284-.0615l4.8303-2.7866a4.4992 4.4992 0 0 1 6.6802 4.66zM8.3065 12.863l-2.02-1.1638a.0804.0804 0 0 1-.038-.0567V6.0742a4.4992 4.4992 0 0 1 7.3757-3.4537l-.142.0805L8.704 5.459a.7948.7948 0 0 0-.3927.6813zm1.0976-2.3654l2.602-1.4998 2.6069 1.4998v2.9994l-2.5974 1.4997-2.6067-1.4997z"
                /></svg
              >
            {:else}
              <svg class="kind" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3"
                ><path d="M3 4l3.5 4L3 12M8 12h5" /></svg
              >
            {/if}

            <span class="name">{title}</span>

            {#if state}
              <span class="runtag {AGENT_LABEL[state].cls}">{AGENT_LABEL[state].label}</span>
            {/if}

            <span
              class="x"
              role="button"
              tabindex="-1"
              aria-label="Close session"
              title="Close session"
              onclick={(e) => {
                e.stopPropagation();
                void closeSession(session);
              }}
              onkeydown={() => {}}>×</span
            >
          </button>
        {/snippet}
      </ContextMenu.Trigger>
      <ContextMenu.Portal>
        <ContextMenu.Content class="menu">
          <ContextMenu.Item class="mi danger" onSelect={() => void closeSession(session)}>
            <svg class="mi-ico" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3"
              ><path d="M4 4l8 8M12 4l-8 8" /></svg
            >
            Close session<span class="mi-k">⌘W</span>
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
      <svg class="kind" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3"
        ><rect x="2.5" y="2.5" width="11" height="11" rx="2" /><line x1="2.5" y1="6.5" x2="13.5" y2="6.5" /></svg
      >
      <span class="name">{diffName}</span>
      <span
        class="x"
        role="button"
        tabindex="-1"
        aria-label="Close diff"
        title="Close diff"
        onclick={(e) => {
          e.stopPropagation();
          closeDiff();
        }}
        onkeydown={() => {}}>×</span
      >
    </button>
  {/if}

  <DropdownMenu.Root>
    <DropdownMenu.Trigger>
      {#snippet child({ props })}
        <button {...props} class="tab-add" title="New session" aria-label="New session">+</button>
      {/snippet}
    </DropdownMenu.Trigger>
    <DropdownMenu.Portal>
      <DropdownMenu.Content class="menu" align="start" sideOffset={6}>
        <DropdownMenu.Item class="mi" onSelect={() => void openSession(parent, "claude", ["claude"])}>
          <!-- Anthropic / Claude sunburst -->
          <svg class="mi-ico claude" viewBox="0 0 16 16" fill="none" stroke="currentColor"
            stroke-width="1.5" stroke-linecap="round"
            ><line x1="8" y1="2.4" x2="8" y2="13.6" /><line x1="2.4" y1="8" x2="13.6" y2="8" /><line
              x1="4.05"
              y1="4.05"
              x2="11.95"
              y2="11.95"
            /><line x1="11.95" y1="4.05" x2="4.05" y2="11.95" /></svg
          >
          Claude
        </DropdownMenu.Item>
        <DropdownMenu.Item class="mi" onSelect={() => void openSession(parent, "codex", ["codex"])}>
          <!-- OpenAI / Codex logomark -->
          <svg class="mi-ico" viewBox="0 0 24 24" fill="currentColor"
            ><path
              d="M22.2819 9.8211a5.9847 5.9847 0 0 0-.5157-4.9108 6.0462 6.0462 0 0 0-6.5098-2.9A6.0651 6.0651 0 0 0 4.9807 4.1818a5.9847 5.9847 0 0 0-3.9977 2.9 6.0462 6.0462 0 0 0 .7427 7.0966 5.98 5.98 0 0 0 .511 4.9107 6.051 6.051 0 0 0 6.5146 2.9001A5.9847 5.9847 0 0 0 13.2599 24a6.0557 6.0557 0 0 0 5.7718-4.2058 5.9894 5.9894 0 0 0 3.9977-2.9001 6.0557 6.0557 0 0 0-.7475-7.0729zm-9.022 12.6081a4.4755 4.4755 0 0 1-2.8764-1.0408l.1419-.0804 4.7783-2.7582a.7948.7948 0 0 0 .3927-.6813v-6.7369l2.02 1.1686a.071.071 0 0 1 .038.052v5.5826a4.504 4.504 0 0 1-4.5165 4.4924zm-9.6607-4.1254a4.4708 4.4708 0 0 1-.5346-3.0137l.1419.0852 4.783 2.7582a.7712.7712 0 0 0 .7806 0l5.8428-3.3685v2.3324a.0804.0804 0 0 1-.0332.0615L9.74 19.9502a4.4992 4.4992 0 0 1-6.1408-1.6464zM2.3408 7.8956a4.485 4.485 0 0 1 2.3655-1.9728V11.6a.7664.7664 0 0 0 .3879.6765l5.8144 3.3543-2.0201 1.1685a.0757.0757 0 0 1-.071 0l-4.8303-2.7865A4.504 4.504 0 0 1 2.3408 7.872zm16.5963 3.8558L13.1038 8.364 15.1192 7.2a.0757.0757 0 0 1 .071 0l4.8303 2.7913a4.4944 4.4944 0 0 1-.6765 8.1042v-5.6772a.79.79 0 0 0-.407-.667zm2.0107-3.0231l-.142-.0852-4.7735-2.7818a.7759.7759 0 0 0-.7854 0L9.409 9.2297V6.8974a.0662.0662 0 0 1 .0284-.0615l4.8303-2.7866a4.4992 4.4992 0 0 1 6.6802 4.66zM8.3065 12.863l-2.02-1.1638a.0804.0804 0 0 1-.038-.0567V6.0742a4.4992 4.4992 0 0 1 7.3757-3.4537l-.142.0805L8.704 5.459a.7948.7948 0 0 0-.3927.6813zm1.0976-2.3654l2.602-1.4998 2.6069 1.4998v2.9994l-2.5974 1.4997-2.6067-1.4997z"
            /></svg
          >
          Codex
        </DropdownMenu.Item>
        <DropdownMenu.Separator class="m-sep" />
        <DropdownMenu.Item class="mi" onSelect={() => void openSession(parent, "shell", null)}>
          <svg class="mi-ico" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3"
            ><path d="M3 4l3.5 4L3 12M8 12h5" /></svg
          >
          Shell
        </DropdownMenu.Item>
      </DropdownMenu.Content>
    </DropdownMenu.Portal>
  </DropdownMenu.Root>
</div>

<style>
  .tabs {
    display: flex;
    align-items: stretch;
    height: 36px;
    gap: 0;
    padding: 0 6px;
    background: var(--bg-1);
    border-bottom: 1px solid var(--line);
    overflow-x: auto;
    /* The strip is a single horizontal lane; never let tabs wrap. */
    flex-wrap: nowrap;
    scrollbar-width: thin;
  }
  .tab {
    display: inline-flex;
    align-items: center;
    /* No vertical padding — the tab stretches to the strip height and the row
       is centred, so the icon/name/× share one baseline. */
    gap: 7px;
    padding: 0 11px;
    font: inherit;
    font-size: 11.5px;
    color: var(--tx-md);
    cursor: pointer;
    background: transparent;
    border: 0;
    /* Tabs are delineated by a vertical divider, not a boxed pill. */
    border-right: 1px solid var(--line-soft);
    white-space: nowrap;
    flex: none;
    transition:
      color var(--t-fast),
      background var(--t-fast);
  }
  .tab:hover {
    color: var(--tx-hi);
    background: var(--bg-2);
  }
  .tab.active {
    /* The active tab takes the terminal's own background so it reads as the
       surface the panel below belongs to. */
    color: var(--tx-hi);
    background: var(--bg-0);
  }
  .tab:focus-visible {
    outline: 2px solid var(--ac);
    outline-offset: -2px;
  }
  .tab .name {
    font-family: var(--mono);
  }
  .runtag {
    font-size: 10px;
    font-weight: 600;
    letter-spacing: 0.2px;
    color: var(--run);
  }
  .runtag.run {
    color: var(--run);
  }
  .runtag.approval {
    color: var(--warn);
  }
  .runtag.done {
    color: var(--ok);
  }
  .runtag.error {
    color: var(--err);
  }
  .tab .x {
    color: var(--tx-lo);
    font-size: 14px;
    line-height: 1;
    width: 16px;
    height: 16px;
    border-radius: 4px;
    display: grid;
    place-items: center;
    /* Hidden until the tab is hovered/active or the × itself is focused, to
       keep idle tabs uncluttered — kept in the DOM (opacity, not display) so
       it stays keyboard/focus reachable. */
    opacity: 0;
    transition:
      opacity var(--t-fast),
      background var(--t-fast),
      color var(--t-fast);
  }
  .tab:hover .x,
  .tab.active .x,
  .tab .x:focus-visible {
    opacity: 1;
  }
  .tab .x:hover {
    background: var(--bg-4);
    color: var(--tx-hi);
  }
  .tab .kind {
    width: 14px;
    height: 14px;
    color: var(--tx-lo);
    flex: none;
  }
  /* The Claude sunburst carries its brand-orange in the tab, as in the menu. */
  .tab .kind.claude {
    color: var(--warn);
  }
  .tab-add {
    display: grid;
    place-items: center;
    width: 30px;
    align-self: stretch;
    margin-left: 4px;
    color: var(--tx-lo);
    cursor: pointer;
    background: transparent;
    border: 0;
    font-size: 16px;
    line-height: 1;
    flex: none;
    transition:
      color var(--t-fast),
      background var(--t-fast);
  }
  .tab-add:hover,
  .tab-add[data-state="open"] {
    color: var(--tx-hi);
    background: var(--bg-2);
  }
  .tab-add:focus-visible {
    outline: 2px solid var(--ac);
    outline-offset: -2px;
  }
  /* The Claude sunburst carries its brand-orange in the menu. */
  .mi-ico.claude {
    color: var(--warn);
  }
</style>
