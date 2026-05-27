<script lang="ts">
  // Session tabs (mockup .tabs). One tab per session in the active parent, then
  // — when a changed file is open — a divider and the diff tab as a peer.
  // Agent sessions surface their state as a coloured word (the runtag); plain
  // shells show the terminal glyph. Double-click a name (or the context-menu
  // Rename) to rename, × closes, the trailing + opens another shell.
  import { tick } from "svelte";
  import { ContextMenu } from "bits-ui";
  import {
    activeSession,
    activeSessionId,
    agentStates,
    closeDiff,
    closeSession,
    diffActive,
    diffPath,
    openSession,
    renameSession,
    visibleSessions,
  } from "../daemon";
  import { AGENT_LABEL, type Id, type Session, type SessionParent } from "../types";

  let { parent }: { parent: SessionParent } = $props();

  let editingId = $state<Id | null>(null);
  let draft = $state("");
  let editEl = $state<HTMLInputElement | null>(null);

  const diffName = $derived($diffPath?.split("/").pop() ?? "diff");

  function select(session: Session) {
    diffActive.set(false);
    activeSessionId.set(session.id);
  }

  async function beginRename(session: Session) {
    editingId = session.id;
    draft = session.name;
    await tick();
    editEl?.focus();
    editEl?.select();
  }

  function commitRename(session: Session) {
    if (editingId !== session.id) return;
    const next = draft;
    editingId = null;
    void renameSession(session, next);
  }

  function onEditKey(event: KeyboardEvent, session: Session) {
    if (event.key === "Enter") {
      event.preventDefault();
      commitRename(session);
    } else if (event.key === "Escape") {
      event.preventDefault();
      editingId = null;
    }
  }
</script>

<div class="tabs" role="tablist">
  {#each $visibleSessions as session (session.id)}
    {@const state = $agentStates[session.id]}
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
            title={session.name}
            onclick={() => select(session)}
            ondblclick={() => beginRename(session)}
          >
            {#if state}
              <span class="runtag {AGENT_LABEL[state].cls}">{AGENT_LABEL[state].label}</span>
            {:else}
              <svg class="kind" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3"
                ><path d="M3 4l3.5 4L3 12M8 12h5" /></svg
              >
            {/if}

            {#if editingId === session.id}
              <!-- svelte-ignore a11y_autofocus -->
              <input
                class="rename"
                bind:this={editEl}
                bind:value={draft}
                onclick={(e) => e.stopPropagation()}
                onkeydown={(e) => onEditKey(e, session)}
                onblur={() => commitRename(session)}
              />
            {:else}
              <span class="name">{session.name}</span>
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
          <ContextMenu.Item class="mi" onSelect={() => beginRename(session)}>
            <svg class="mi-ico" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3"
              ><path d="M11 2.5 13.5 5 6 12.5 3 13l.5-3z" /></svg
            >
            Rename…<span class="mi-k">⏎</span>
          </ContextMenu.Item>
          <ContextMenu.Separator class="m-sep" />
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
    <span class="tab-div"></span>
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

  <button
    class="tab-add"
    title="New shell session"
    aria-label="New shell session"
    onclick={() => void openSession(parent, "shell", null)}>+</button
  >
</div>

<style>
  .tabs {
    display: flex;
    align-items: stretch;
    gap: 2px;
    padding: 0 8px;
    background: var(--bg-1);
    border-bottom: 1px solid var(--line);
    overflow-x: auto;
  }
  .tab {
    display: inline-flex;
    align-items: center;
    gap: 7px;
    padding: 8px 10px 9px;
    font: inherit;
    font-size: 11.5px;
    color: var(--tx-md);
    cursor: pointer;
    background: transparent;
    border: 0;
    border-bottom: 2px solid transparent;
    white-space: nowrap;
    transition:
      color var(--t-fast),
      border-color var(--t-fast);
  }
  .tab:hover {
    color: var(--tx-hi);
  }
  .tab.active {
    color: var(--tx-hi);
    border-bottom-color: var(--ac);
  }
  .tab .name {
    font-family: var(--mono);
  }
  .tab .rename {
    font-family: var(--mono);
    font-size: 11.5px;
    color: var(--tx-hi);
    background: var(--bg-0);
    border: 1px solid var(--ac);
    border-radius: 4px;
    padding: 0 4px;
    width: 9ch;
    outline: none;
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
    font-size: 13px;
    width: 14px;
    height: 14px;
    border-radius: 3px;
    display: grid;
    place-items: center;
  }
  .tab .x:hover {
    background: var(--bg-4);
    color: var(--tx-hi);
  }
  .tab .kind {
    width: 14px;
    height: 14px;
    color: var(--tx-lo);
  }
  .tab-div {
    width: 1px;
    background: var(--line);
    margin: 7px 4px;
    flex: none;
  }
  .tab-add {
    padding: 8px 9px;
    color: var(--tx-lo);
    cursor: pointer;
    background: 0;
    border: 0;
    font-size: 16px;
  }
  .tab-add:hover {
    color: var(--tx-hi);
  }
</style>
