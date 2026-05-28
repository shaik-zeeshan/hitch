<script lang="ts">
  // Route `/settings` — full-window preferences page (peer of the `/` shell, not
  // an overlay). Replaces the old SettingsDialog modal. A back control returns to
  // the shell via client-side navigation, so the daemon connection (owned by the
  // root layout) is never torn down. The left sub-nav is built to grow; today the
  // sections include Editor, Drafts, and static About — no dead UI.
  import { goto } from "$app/navigation";
  import { listDraftModels } from "$lib/daemon";
  import { onMount } from "svelte";
  import { Select } from "bits-ui";
  import {
    DEFAULT_DRAFT_MODEL,
    DEFAULT_DRAFT_PROVIDER,
    DEFAULT_EDITOR,
    DRAFT_MODEL_OPTIONS,
    draftModel,
    draftProvider,
    editorApp,
    type DraftProvider,
  } from "$lib/settings";

  type Section = "editor" | "drafts" | "about";
  const DEFAULT_MODEL_VALUE = "__hitch_cli_default__";
  const providerOptions: Array<{ value: DraftProvider; label: string }> = [
    { value: "stub", label: "Stub (deterministic)" },
    { value: "claude", label: "Claude" },
    { value: "codex", label: "Codex" },
  ];

  let section = $state<Section>("editor");

  // Staged editor value, committed (trimmed, with fallback) on Save / Enter /
  // blur — mirroring the old dialog so an empty field can't wipe the setting.
  let editor = $state("");
  let draftProviderValue = $state<DraftProvider>(DEFAULT_DRAFT_PROVIDER);
  let selectedDraftModel = $state(DEFAULT_MODEL_VALUE);
  let modelOptions = $state<string[]>(DRAFT_MODEL_OPTIONS[DEFAULT_DRAFT_PROVIDER]);
  let modelsLoading = $state(false);
  let modelsError = $state<string | null>(null);
  let modelLoadSeq = 0;
  let lastDraftProvider: DraftProvider = DEFAULT_DRAFT_PROVIDER;
  let draftsHydrated = false;
  let saved = $state(false);
  let draftSaved = $state(false);
  let selectableModels = $derived(
    selectedDraftModel !== DEFAULT_MODEL_VALUE && !modelOptions.includes(selectedDraftModel)
      ? [selectedDraftModel, ...modelOptions]
      : modelOptions,
  );

  onMount(() => {
    editor = $editorApp;
    // `$draftProvider` is null until the user explicitly picks one; show the
    // default as the editable starting point. Saving it writes a concrete,
    // explicit choice (see settings.ts).
    draftProviderValue = $draftProvider ?? DEFAULT_DRAFT_PROVIDER;
    selectedDraftModel = $draftModel || DEFAULT_MODEL_VALUE;
    lastDraftProvider = draftProviderValue;
    draftsHydrated = true;
  });

  $effect(() => {
    if (draftsHydrated && draftProviderValue !== lastDraftProvider) {
      selectedDraftModel = DEFAULT_MODEL_VALUE;
      lastDraftProvider = draftProviderValue;
    }
    void loadModels(draftProviderValue);
  });

  function commitEditor() {
    const next = editor.trim() || DEFAULT_EDITOR;
    editor = next;
    if (next !== $editorApp) {
      editorApp.set(next);
      saved = true;
      setTimeout(() => (saved = false), 1600);
    }
  }

  async function loadModels(provider: DraftProvider) {
    const seq = ++modelLoadSeq;
    modelsError = null;

    // The stub provider is a local deterministic choice with no CLI; skip the
    // IPC roundtrip. Every other provider's authoritative model list comes from
    // the daemon (`list-draft-models`); DRAFT_MODEL_OPTIONS is the offline
    // fallback used only on error/timeout.
    if (provider === "stub") {
      modelOptions = DRAFT_MODEL_OPTIONS[provider];
      modelsLoading = false;
      return;
    }

    modelsLoading = true;
    try {
      const models = await withTimeout(listDraftModels(provider), 3500);
      if (seq !== modelLoadSeq) return;
      modelOptions = models.length > 0 ? models : DRAFT_MODEL_OPTIONS[provider];
    } catch (err) {
      if (seq !== modelLoadSeq) return;
      const message = err instanceof Error ? err.message : String(err);
      modelOptions = DRAFT_MODEL_OPTIONS[provider];
      modelsError = message.includes("unknown variant `list-draft-models`")
        ? "Restart Hitch to enable live model discovery."
        : message;
    } finally {
      if (seq === modelLoadSeq) modelsLoading = false;
    }
  }

  function withTimeout<T>(promise: Promise<T>, ms: number): Promise<T> {
    return Promise.race([
      promise,
      new Promise<T>((_, reject) =>
        setTimeout(() => reject(new Error("model discovery timed out; using fallback list")), ms),
      ),
    ]);
  }

  function commitDraftSettings() {
    draftProvider.set(draftProviderValue);
    draftModel.set(selectedDraftModel === DEFAULT_MODEL_VALUE ? DEFAULT_DRAFT_MODEL : selectedDraftModel);
    draftSaved = true;
    setTimeout(() => (draftSaved = false), 1600);
  }

  function back() {
    void goto("/");
  }

  function onKeydown(event: KeyboardEvent) {
    if (event.key === "Escape") back();
  }
</script>

<svelte:window on:keydown={onKeydown} />

<div class="settings">
  <header class="s-head">
    <button class="back" onclick={back} aria-label="Back to workspace">
      <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5"
        ><path d="M10 3 5 8l5 5" stroke-linecap="round" stroke-linejoin="round" /></svg
      >
      Back
    </button>
    <h1>Settings</h1>
  </header>

  <div class="s-body">
    <nav class="s-nav" aria-label="Settings sections">
      <button class="nav-row" class:active={section === "editor"} onclick={() => (section = "editor")}>
        Editor
      </button>
      <button class="nav-row" class:active={section === "drafts"} onclick={() => (section = "drafts")}>
        Drafts
      </button>
      <button class="nav-row" class:active={section === "about"} onclick={() => (section = "about")}>
        About
      </button>
    </nav>

    <div class="s-content">
      {#if section === "editor"}
        <section class="panel">
          <div class="panel-head">
            <h2>Editor</h2>
            <p class="help">
              Application used by <b>Open in editor</b> on a worktree. Any installed editor by name —
              e.g. <span class="mono">Visual Studio Code</span>, <span class="mono">Cursor</span>,
              <span class="mono">Zed</span>.
            </p>
          </div>
          <label class="field">
            <span>Editor application</span>
            <input
              class="base"
              bind:value={editor}
              placeholder={DEFAULT_EDITOR}
              onblur={commitEditor}
              onkeydown={(e) => e.key === "Enter" && commitEditor()}
            />
          </label>
          <div class="row">
            <button class="btn primary" onclick={commitEditor}>Save</button>
            {#if saved}<span class="saved" role="status">Saved</span>{/if}
          </div>
        </section>
      {:else if section === "drafts"}
        <section class="panel">
          <div class="panel-head">
            <h2>Drafts</h2>
            <p class="help">
              Provider and model used by <b>Generate</b> in commit and pull-request dialogs.
              Claude and Codex run headlessly through their installed CLIs.
            </p>
          </div>
          <div class="field">
            <span>Provider</span>
            <Select.Root type="single" bind:value={draftProviderValue}>
              <Select.Trigger class="select-trigger base" aria-label="Draft provider">
                <Select.Value placeholder="Choose provider" />
                <span class="select-chev" aria-hidden="true">⌄</span>
              </Select.Trigger>
              <Select.Portal>
                <Select.Content class="select-content" sideOffset={6}>
                  <Select.Viewport>
                    {#each providerOptions as option}
                      <Select.Item class="select-item" value={option.value} label={option.label}>
                        {option.label}
                      </Select.Item>
                    {/each}
                  </Select.Viewport>
                </Select.Content>
              </Select.Portal>
            </Select.Root>
          </div>
          <div class="field">
            <span>Model</span>
            <Select.Root type="single" bind:value={selectedDraftModel}>
              <Select.Trigger class="select-trigger base" aria-label="Draft model">
                <Select.Value placeholder={modelsLoading ? "Loading models…" : "Provider CLI default"} />
                <span class="select-chev" aria-hidden="true">⌄</span>
              </Select.Trigger>
              <Select.Portal>
                <Select.Content class="select-content" sideOffset={6}>
                  <Select.Viewport>
                    <Select.Item class="select-item" value={DEFAULT_MODEL_VALUE} label="Provider CLI default">
                      Provider CLI default
                    </Select.Item>
                    {#each selectableModels as model}
                      <Select.Item class="select-item" value={model} label={model}>
                        {model}
                      </Select.Item>
                    {/each}
                  </Select.Viewport>
                </Select.Content>
              </Select.Portal>
            </Select.Root>
          </div>
          <p class="help">
            {#if modelsLoading}
              Loading models…
            {:else if modelsError}
              Using bundled fallback models: <span class="mono">{modelsError}</span>
            {:else if draftProviderValue === "codex"}
              Models are loaded with <span class="mono">codex debug models</span>. Choose default to omit <span class="mono">--model</span>.
            {:else if draftProviderValue === "claude"}
              Models are loaded from the Claude CLI. Choose default to omit <span class="mono">--model</span>.
            {:else}
              Choose default to omit <span class="mono">--model</span>.
            {/if}
          </p>
          <div class="row">
            <button class="btn primary" onclick={commitDraftSettings}>Save</button>
            {#if draftSaved}<span class="saved" role="status">Saved</span>{/if}
          </div>
        </section>
      {:else if section === "about"}
        <section class="panel">
          <div class="panel-head">
            <h2>About</h2>
          </div>
          <dl class="about">
            <div><dt>App</dt><dd>Hitch</dd></div>
            <div><dt>Version</dt><dd class="mono">0.1.0</dd></div>
          </dl>
          <p class="help">
            A focused workspace for git worktrees and the agents you run in them.
          </p>
        </section>
      {/if}
    </div>
  </div>
</div>

<style>
  .settings {
    height: 100%;
    width: 100%;
    display: grid;
    grid-template-rows: 52px 1fr;
    background: var(--bg-1);
    overflow: hidden;
  }

  .s-head {
    display: flex;
    align-items: center;
    gap: 14px;
    padding: 0 16px;
    border-bottom: 1px solid var(--line);
    /* leave room for the macOS traffic lights (overlay title bar) */
    padding-left: 88px;
    -webkit-app-region: drag;
  }
  .s-head h1 {
    font-size: 13px;
    font-weight: 600;
    color: var(--tx-hi);
    margin: 0;
  }
  .back {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    padding: 5px 10px 5px 7px;
    border-radius: var(--radius);
    background: var(--bg-3);
    border: 1px solid var(--line);
    color: var(--tx-md);
    font: inherit;
    font-size: 11.5px;
    font-weight: 540;
    cursor: pointer;
    -webkit-app-region: no-drag;
    transition:
      background var(--t-fast),
      color var(--t-fast);
  }
  .back:hover {
    background: var(--bg-4);
    color: var(--tx-hi);
  }
  .back svg {
    width: 13px;
    height: 13px;
  }

  .s-body {
    display: grid;
    grid-template-columns: 200px 1fr;
    min-height: 0;
  }

  .s-nav {
    background: var(--bg-2);
    border-right: 1px solid var(--line);
    padding: 12px 8px;
    display: grid;
    align-content: start;
    gap: 1px;
    overflow-y: auto;
  }
  .nav-row {
    text-align: left;
    padding: 7px 10px;
    border-radius: var(--radius);
    background: transparent;
    border: 1px solid transparent;
    color: var(--tx-md);
    font: inherit;
    font-size: 12px;
    cursor: pointer;
    transition:
      background var(--t-fast),
      color var(--t-fast);
  }
  .nav-row:hover {
    background: var(--bg-3);
    color: var(--tx-hi);
  }
  .nav-row.active {
    background: var(--ac-wash);
    color: var(--tx-hi);
  }

  .s-content {
    overflow-y: auto;
    min-height: 0;
    padding: 28px 32px;
  }
  .panel {
    max-width: 520px;
    display: grid;
    gap: 16px;
  }
  .panel-head {
    display: grid;
    gap: 6px;
  }
  .panel-head h2 {
    font-size: 15px;
    font-weight: 600;
    color: var(--tx-hi);
    margin: 0;
  }
  .panel-head .help b {
    color: var(--tx-md);
    font-weight: 540;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 12px;
  }
  .saved {
    font-size: 11.5px;
    color: var(--tx-lo);
  }

  .about {
    display: grid;
    gap: 9px;
    margin: 0;
  }
  .about > div {
    display: grid;
    grid-template-columns: 90px 1fr;
    align-items: baseline;
  }
  .about dt {
    font-size: 11px;
    color: var(--tx-lo);
  }
  .about dd {
    margin: 0;
    font-size: 12.5px;
    color: var(--tx-hi);
  }
  .about dd.mono {
    font-family: var(--mono);
  }
</style>
