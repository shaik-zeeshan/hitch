<script lang="ts">
  // Remote folder browser (issue #28, ADR 0014). Adding a Project inside an SSH
  // Host scope opens this folders-first directory browser backed by
  // `list-directory` requests routed to that host's Daemon. The browser opens at
  // the remote daemon user's home directory, navigates any readable directory,
  // toggles hidden folders (off by default), and accepts a typed absolute path.
  // Confirming an existing remote folder sends AddProject to that host's daemon;
  // the returned/broadcast Project lands under the SSH Host scope (issue #27
  // event path). The GUI never maps remote paths onto local paths.
  //
  // A target-scope select (Local + connected hosts) lets a globally-invoked Add
  // Project change scope; defaulting to the selected scope. Local stays on the
  // native folder picker (zero regression) — the dialog offers that button rather
  // than a remote browser when Local is the target. Reuses the shared Paper
  // Terminal dialog vocabulary (.modal/.field/.btn/list rows).
  import { Dialog } from "bits-ui";
  import ChevronRight from "~icons/lucide/chevron-right";
  import Folder from "~icons/lucide/folder";
  import HomeIcon from "~icons/lucide/house";
  import ArrowUp from "~icons/lucide/arrow-up";
  import {
    addProject,
    daemonScopesOrdered,
    listDirectory,
    pickAndAddProject,
  } from "../daemon";
  import { remoteBrowserScope } from "../overlays";
  import { LOCAL_SCOPE_ID, type DaemonScopeId, type DirEntry } from "../types";

  let scopeId = $state<DaemonScopeId>(LOCAL_SCOPE_ID);
  let currentPath = $state<string>("");
  let parentPath = $state<string | null>(null);
  let homePath = $state<string | null>(null);
  let entries = $state<DirEntry[]>([]);
  let showHidden = $state(false);
  let loading = $state(false);
  let errMsg = $state<string | null>(null);
  let adding = $state(false);
  // The editable path bar: bound to the input, only committed on Enter so typing
  // does not refetch on every keystroke.
  let pathInput = $state<string>("");
  let activeIndex = $state(-1);

  const isLocal = $derived(scopeId === LOCAL_SCOPE_ID);
  // Only connected/known scopes are offered. Local is always present; SSH Hosts
  // appear as the tree learns them. A confirmed remote folder needs a live path,
  // so the browser only renders when a valid current path has loaded.
  const scopes = $derived($daemonScopesOrdered);
  const scopeLabel = $derived(
    scopes.find((s) => s.id === scopeId)?.label ?? scopeId,
  );
  const canAdd = $derived(!adding && !loading && !isLocal && currentPath.length > 0);

  function onOpenChange(next: boolean) {
    if (!next) remoteBrowserScope.set(null);
  }

  // Each open seeds the target scope from the store and (for a remote target)
  // loads the home directory. `$effect` reacts to the store opening.
  $effect(() => {
    const initial = $remoteBrowserScope;
    if (initial === null) return;
    // Seed scope + reset transient state once per open. Guard so the effect does
    // not re-run on internal state changes (it only depends on the store).
    scopeId = initial;
    errMsg = null;
    adding = false;
    activeIndex = -1;
    if (initial !== LOCAL_SCOPE_ID) {
      void load(null);
    } else {
      entries = [];
      currentPath = "";
      parentPath = null;
      homePath = null;
      pathInput = "";
    }
  });

  // Switching the target scope in the select: a remote scope loads its home; Local
  // clears the browser and offers the native picker.
  function onScopeChange(next: DaemonScopeId) {
    scopeId = next;
    errMsg = null;
    activeIndex = -1;
    if (next === LOCAL_SCOPE_ID) {
      entries = [];
      currentPath = "";
      parentPath = null;
      homePath = null;
      pathInput = "";
    } else {
      void load(null);
    }
  }

  // Fetch a directory listing from the target host's daemon. `path: null` opens
  // home. A failure renders an explicit error row without clearing the last good
  // path bar, so the user can retype.
  async function load(path: string | null) {
    if (isLocal) return;
    loading = true;
    errMsg = null;
    activeIndex = -1;
    try {
      const listing = await listDirectory(path, showHidden, scopeId);
      currentPath = listing.path;
      parentPath = listing.parent;
      homePath = listing.home;
      entries = listing.entries;
      pathInput = listing.path;
    } catch (err) {
      errMsg = err instanceof Error ? err.message : String(err);
    } finally {
      loading = false;
    }
  }

  function descend(entry: DirEntry) {
    void load(entry.path);
  }

  function goParent() {
    if (parentPath) void load(parentPath);
  }

  function goHome() {
    void load(null);
  }

  // Commit the typed absolute path (Enter in the path bar). An invalid path
  // surfaces as an error row from the daemon (NotFound/Unauthorized).
  function jumpToTyped() {
    const next = pathInput.trim();
    if (next.length > 0) void load(next);
  }

  function toggleHidden() {
    showHidden = !showHidden;
    // Re-request the SAME directory with the new hidden setting.
    void load(currentPath || null);
  }

  async function confirmAdd() {
    if (!canAdd) return;
    adding = true;
    errMsg = null;
    try {
      await addProject(currentPath, scopeId);
      remoteBrowserScope.set(null);
    } catch (err) {
      errMsg = err instanceof Error ? err.message : String(err);
    } finally {
      adding = false;
    }
  }

  async function useNativePicker() {
    await pickAndAddProject();
    remoteBrowserScope.set(null);
  }

  // Arrow navigation over the folder list (parent row counts as index -1 visually
  // but we keep keyboard focus simple: Up/Down move within entries, Enter descends).
  function onListKey(e: KeyboardEvent) {
    if (entries.length === 0) return;
    if (e.key === "ArrowDown") {
      e.preventDefault();
      activeIndex = Math.min(activeIndex + 1, entries.length - 1);
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      activeIndex = Math.max(activeIndex - 1, 0);
    } else if (e.key === "Enter" && activeIndex >= 0) {
      e.preventDefault();
      descend(entries[activeIndex]);
    }
  }
</script>

<Dialog.Root open={$remoteBrowserScope !== null} {onOpenChange}>
  <Dialog.Portal>
    <Dialog.Overlay class="modal-back" />
    <Dialog.Content class="modal browser" aria-describedby={undefined}>
      <div class="m-head">
        <Dialog.Title>Add a project</Dialog.Title>
        <div class="sub">Browse a folder on the target daemon, then add it as a project.</div>
      </div>
      <div class="m-body">
        <label class="field">
          <span>Target daemon</span>
          <select
            class="base"
            value={scopeId}
            onchange={(e) => onScopeChange((e.currentTarget as HTMLSelectElement).value)}
          >
            {#each scopes as scope (scope.id)}
              <option value={scope.id}>{scope.label}</option>
            {/each}
          </select>
        </label>

        {#if isLocal}
          <p class="help">
            Local projects use the native folder picker.
          </p>
          <div class="local-pick">
            <button type="button" class="btn" onclick={() => void useNativePicker()}>
              <Folder class="mi-ico icon" />
              Choose a local folder…
            </button>
          </div>
        {:else}
          <div class="path-bar">
            <button
              type="button"
              class="nav"
              title="Home"
              aria-label="Home"
              disabled={loading}
              onclick={goHome}
            >
              <HomeIcon class="icon" />
            </button>
            <button
              type="button"
              class="nav"
              title="Parent folder"
              aria-label="Parent folder"
              disabled={loading || !parentPath}
              onclick={goParent}
            >
              <ArrowUp class="icon" />
            </button>
            <!-- svelte-ignore a11y_autofocus -->
            <input
              class="base path-input"
              bind:value={pathInput}
              placeholder="/absolute/remote/path"
              spellcheck="false"
              autofocus
              onkeydown={(e) => e.key === "Enter" && jumpToTyped()}
            />
            <button
              type="button"
              class="hidden-toggle"
              class:on={showHidden}
              title={showHidden ? "Hide hidden folders" : "Show hidden folders"}
              aria-pressed={showHidden}
              onclick={toggleHidden}
            >
              .*
            </button>
          </div>

          <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
          <ul class="folders" role="listbox" tabindex="0" onkeydown={onListKey}>
            {#if loading}
              <li class="state-row loading">Loading…</li>
            {:else if errMsg}
              <li class="state-row error">{errMsg}</li>
            {:else if entries.length === 0}
              <li class="state-row empty">No subfolders here.</li>
            {:else}
              {#each entries as entry, i (entry.path)}
                <li>
                  <button
                    type="button"
                    class="folder-row"
                    class:active={i === activeIndex}
                    onclick={() => descend(entry)}
                  >
                    <Folder class="folder-ic icon" />
                    <span class="folder-name">{entry.name}</span>
                    <ChevronRight class="chev icon" />
                  </button>
                </li>
              {/each}
            {/if}
          </ul>
        {/if}

        {#if errMsg && !isLocal && !loading && entries.length > 0}
          <p class="m-error">{errMsg}</p>
        {/if}
      </div>
      <div class="m-foot">
        <Dialog.Close class="btn">Cancel</Dialog.Close>
        {#if !isLocal}
          <button class="btn primary" disabled={!canAdd} onclick={() => void confirmAdd()}>
            {adding ? "Adding…" : "Add project"}
          </button>
        {/if}
      </div>
    </Dialog.Content>
  </Dialog.Portal>
</Dialog.Root>

<style>
  /* A slightly wider modal so the folder list has room. The class rides
     bits-ui's Dialog.Content (forwarded), so it needs :global to bind. Reuses
     the shared .modal frame from the global stylesheet. */
  :global(.modal.browser) {
    width: min(520px, calc(100vw - 32px));
  }

  .path-bar {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .path-input {
    flex: 1 1 auto;
    min-width: 0;
    font-family: var(--mono);
    font-size: var(--r0);
  }
  .nav,
  .hidden-toggle {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    flex: 0 0 auto;
    width: 28px;
    height: 28px;
    border: 1px solid var(--line);
    background: var(--paper-2);
    color: var(--ink-2);
    cursor: pointer;
  }
  .nav:disabled {
    opacity: 0.4;
    cursor: default;
  }
  .nav:not(:disabled):hover,
  .hidden-toggle:hover {
    background: var(--paper-3);
    color: var(--ink-1);
  }
  .hidden-toggle {
    font-family: var(--mono);
    font-size: 0.6875rem;
    letter-spacing: 0.04em;
  }
  .hidden-toggle.on {
    background: var(--st-ok-glow);
    border-color: var(--st-ok);
    color: var(--ink-1);
  }

  .folders {
    list-style: none;
    margin: 8px 0 0;
    padding: 0;
    max-height: 320px;
    min-height: 120px;
    overflow-y: auto;
    border: 1px solid var(--line);
    background: var(--paper-2);
  }
  .folders:focus-visible {
    outline: 2px solid var(--st-ok);
    outline-offset: -2px;
  }

  .folder-row {
    display: flex;
    align-items: center;
    gap: 8px;
    width: 100%;
    padding: 6px 10px;
    border: 0;
    background: transparent;
    color: var(--ink-1);
    cursor: pointer;
    text-align: left;
    font-size: var(--r0);
  }
  .folder-row:hover,
  .folder-row.active {
    background: var(--paper-3);
  }
  .folder-name {
    flex: 1 1 auto;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .folder-row :global(.chev) {
    flex: 0 0 auto;
    color: var(--ink-3);
  }

  .state-row {
    padding: 10px 12px;
    font-size: var(--r0);
    color: var(--ink-2);
  }
  .state-row.error {
    color: var(--st-need);
    background: var(--st-need-wash);
  }
  .state-row.loading {
    color: var(--ink-3);
    font-style: italic;
  }

  .local-pick {
    margin-top: 8px;
  }
</style>
