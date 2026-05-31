<script lang="ts">
  // Projects → Worktrees tree (mockup .tree). Project rows read as group
  // headers; their worktrees nest under an indent guide with connector ticks.
  // Agent state shows as a WORD in a reserved hue (rolled up to the project row
  // when collapsed). Dirty worktrees show their aggregate +/− line stat next
  // to the branch name.
  import { ContextMenu } from "bits-ui";
  import { openPath, revealItemInDir } from "@tauri-apps/plugin-opener";
  import { get } from "svelte/store";
  import { DEFAULT_EDITOR, editorApp } from "../settings";
  import {
    agentStateByProject,
    agentStateByWorktree,
    dirtyWorktrees,
    openSession,
    projects,
    selectedProjectId,
    selectedWorktreeId,
    worktreeLineStats,
    worktrees,
  } from "../daemon";
  import { createWorktreeFor, removeProjectTarget, removeWorktreeTarget } from "../overlays";
  import { AGENT_LABEL, type Id, type Project, type Worktree } from "../types";

  // Open a session under a worktree, selecting it first so the new session
  // lands in view. Used by the worktree context menu's launch items.
  function launch(worktree: Worktree, command: string[] | null, name: string) {
    selectWorktree(worktree);
    void openSession({ kind: "worktree", id: worktree.id }, name, command);
  }

  async function copyPath(path: string) {
    try {
      await navigator.clipboard.writeText(path);
    } catch {
      // Clipboard can be unavailable (no focus / denied); a silent no-op is
      // better than surfacing a scary error for a convenience action.
    }
  }

  // Reveal the worktree in the OS file manager (Finder) via the opener plugin.
  async function revealInFinder(path: string) {
    try {
      await revealItemInDir(path);
    } catch (err) {
      // No file manager / denied — log and no-op rather than crash the menu.
      console.error("Reveal in Finder failed:", err);
    }
  }

  // Open the worktree in the configured editor (settings.ts). openPath's second
  // arg is the OS application name, so an uninstalled/misnamed editor just
  // fails gracefully — the context menu has no inline error surface.
  async function openInEditor(path: string) {
    try {
      await openPath(path, get(editorApp).trim() || DEFAULT_EDITOR);
    } catch (err) {
      console.error("Open in editor failed:", err);
    }
  }

  // Per-project expand state; git projects start expanded so worktrees show.
  let collapsed = $state<Record<Id, boolean>>({});

  const worktreesFor = (projectId: Id) =>
    $worktrees.filter((w) => w.project_id === projectId);

  function isExpanded(p: Project): boolean {
    return p.kind === "git-backed" && !collapsed[p.id];
  }

  function toggleExpand(p: Project) {
    collapsed = { ...collapsed, [p.id]: !collapsed[p.id] };
  }

  // Clicking a project row selects it (for ⌘K context + the quick-add target)
  // and toggles its worktree list. It MUST also clear the selected worktree so
  // even re-clicking the SAME project row returns the UI to the project-level
  // “choose a worktree” state instead of leaving the prior branch selected.
  function selectProject(p: Project) {
    selectedProjectId.set(p.id);
    selectedWorktreeId.set(null);
    if (p.kind === "git-backed") toggleExpand(p);
  }

  function selectWorktree(w: Worktree) {
    // Set the project too: the tree shows every project's worktrees at once,
    // and the selected worktree must belong to the selected project.
    selectedProjectId.set(w.project_id);
    selectedWorktreeId.set(w.id);
  }

  function onProjectKey(event: KeyboardEvent, p: Project) {
    if (event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      selectProject(p);
    }
  }
</script>

<div class="tree">
  <div class="sec-head">Projects</div>

  {#if $projects.length === 0}
    <p class="empty-copy">No projects yet. Add a local repo or folder to begin.</p>
  {/if}

  {#each $projects as project (project.id)}
    {@const status = $agentStateByProject[project.id]}
    {@const expanded = isExpanded(project)}
    <ContextMenu.Root>
      <ContextMenu.Trigger>
        {#snippet child({ props })}
          <div
            {...props}
            class="row"
            class:sel={project.id === $selectedProjectId && $selectedWorktreeId === null}
            role="button"
            tabindex="0"
            onclick={() => selectProject(project)}
            onkeydown={(e) => onProjectKey(e, project)}
          >
            {#if project.kind === "git-backed"}
              <button
                class="twirl"
                class:open={expanded}
                aria-label={expanded ? "Collapse" : "Expand"}
                onclick={(e) => {
                  e.stopPropagation();
                  toggleExpand(project);
                }}
              >
                <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"
                  ><path d="M6 4l4 4-4 4" /></svg
                >
              </button>
            {:else}
              <span class="twirl"></span>
            {/if}

            {#if project.kind === "git-backed"}
              <svg class="ico" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3"
                ><circle cx="4" cy="4" r="1.8" /><circle cx="4" cy="12" r="1.8" /><circle cx="12" cy="6" r="1.8" /><path
                  d="M4 5.8v4.4M5.7 4.5c3 0 4.6 0 5.3 0M11 7.7c0 1.5-1.4 2.4-3.2 2.4H5.8"
                /></svg
              >
            {:else}
              <svg class="ico" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3"
                ><path d="M1.5 4.5a2 2 0 0 1 2-2h3l1.5 1.6h4.5a2 2 0 0 1 2 2v5.4a2 2 0 0 1-2 2h-9a2 2 0 0 1-2-2z" /></svg
              >
            {/if}

            <span class="lbl">{project.name}</span>

            {#if !expanded && status}
              <span class="status {AGENT_LABEL[status].cls}">{AGENT_LABEL[status].label}</span>
            {/if}

            {#if project.kind === "git-backed"}
              <button
                class="quick-add"
                aria-label={`New worktree in ${project.name}`}
                title={`New worktree in ${project.name}`}
                onclick={(e) => {
                  e.stopPropagation();
                  createWorktreeFor.set(project);
                }}
              >
                <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round"
                  ><path d="M8 3.5v9M3.5 8h9" /></svg
                >
              </button>
            {/if}
          </div>
        {/snippet}
      </ContextMenu.Trigger>
      <ContextMenu.Portal>
        <ContextMenu.Content class="menu">
          {#if project.kind === "git-backed"}
            <ContextMenu.Item class="mi" onSelect={() => createWorktreeFor.set(project)}>
              <svg class="mi-ico" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3"
                ><path d="M8 3.5v9M3.5 8h9" /></svg
              >
              New worktree…
            </ContextMenu.Item>
            <ContextMenu.Separator class="m-sep" />
          {/if}
          <ContextMenu.Item class="mi" onSelect={() => void revealInFinder(project.root)}>
            <svg class="mi-ico" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3"
              ><path d="M1.5 4.5a2 2 0 0 1 2-2h3l1.5 1.6h4.5a2 2 0 0 1 2 2v5.4a2 2 0 0 1-2 2h-9a2 2 0 0 1-2-2z" /></svg
            >
            Reveal in Finder
          </ContextMenu.Item>
          <ContextMenu.Item class="mi" onSelect={() => void openInEditor(project.root)}>
            <svg class="mi-ico" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3"
              ><rect x="2" y="3" width="12" height="10" rx="2" /><path d="M6 6l-2 2 2 2M10 6l2 2-2 2" /></svg
            >
            Open in editor
          </ContextMenu.Item>
          <ContextMenu.Item class="mi" onSelect={() => void copyPath(project.root)}>
            <svg class="mi-ico" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3"
              ><rect x="3" y="3" width="8" height="8" rx="1.5" /><path
                d="M5.5 3V2.2a1 1 0 0 1 1-1H13a1 1 0 0 1 1 1v6.5a1 1 0 0 1-1 1h-.8"
              /></svg
            >
            Copy path
          </ContextMenu.Item>
          <ContextMenu.Separator class="m-sep" />
          <ContextMenu.Item class="mi danger" onSelect={() => removeProjectTarget.set(project)}>
            <svg class="mi-ico" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3"
              ><path
                d="M3 4.5h10M6 4.5V3.2a1 1 0 0 1 1-1h2a1 1 0 0 1 1 1V4.5M4.5 4.5l.6 8a1 1 0 0 0 1 1h3.8a1 1 0 0 0 1-1l.6-8"
              /></svg
            >
            Remove project…
          </ContextMenu.Item>
        </ContextMenu.Content>
      </ContextMenu.Portal>
    </ContextMenu.Root>

    {#if expanded}
      <div class="worktrees">
        {#each worktreesFor(project.id) as worktree (worktree.id)}
          {@const wtStatus = $agentStateByWorktree[worktree.id]}
          {@const lineStat = $worktreeLineStats[worktree.id]}
          <ContextMenu.Root>
            <ContextMenu.Trigger>
              {#snippet child({ props })}
                <button
                  {...props}
                  class="row wt-row"
                  class:sel={worktree.id === $selectedWorktreeId}
                  onclick={() => selectWorktree(worktree)}
                >
                  <span class="branch-main">
                    <span class="lbl br-name">{worktree.branch}</span>
                    {#if lineStat && (lineStat.additions > 0 || lineStat.deletions > 0)}
                      <span
                        class="diffstat"
                        title={`${lineStat.additions} additions, ${lineStat.deletions} deletions`}
                      >
                        {#if lineStat.additions > 0}<span class="add">{lineStat.additions}+</span>{/if}
                        {#if lineStat.deletions > 0}<span class="del">{lineStat.deletions}-</span>{/if}
                      </span>
                    {/if}
                  </span>
                  {#if wtStatus}
                    <span class="meta-r">
                      <span class="status {AGENT_LABEL[wtStatus].cls}">{AGENT_LABEL[wtStatus].label}</span>
                    </span>
                  {/if}
                </button>
              {/snippet}
            </ContextMenu.Trigger>
            <ContextMenu.Portal>
              <ContextMenu.Content class="menu">
                <ContextMenu.Item class="mi" onSelect={() => launch(worktree, null, "shell")}>
                  <svg class="mi-ico" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3"
                    ><path d="M3 4l3.5 4L3 12M8 12h5" /></svg
                  >
                  Open shell session<span class="mi-k">⌘T</span>
                </ContextMenu.Item>
                <ContextMenu.Item class="mi" onSelect={() => launch(worktree, ["claude"], "claude")}>
                  <span class="mi-ico" style="color:var(--warn); display:grid; place-items:center">✳</span>
                  Launch Claude
                </ContextMenu.Item>
                <ContextMenu.Separator class="m-sep" />
                <ContextMenu.Item class="mi" onSelect={() => void revealInFinder(worktree.path)}>
                  <svg class="mi-ico" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3"
                    ><path d="M1.5 4.5a2 2 0 0 1 2-2h3l1.5 1.6h4.5a2 2 0 0 1 2 2v5.4a2 2 0 0 1-2 2h-9a2 2 0 0 1-2-2z" /></svg
                  >
                  Reveal in Finder
                </ContextMenu.Item>
                <ContextMenu.Item class="mi" onSelect={() => void openInEditor(worktree.path)}>
                  <svg class="mi-ico" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3"
                    ><rect x="2" y="3" width="12" height="10" rx="2" /><path d="M6 6l-2 2 2 2M10 6l2 2-2 2" /></svg
                  >
                  Open in editor
                </ContextMenu.Item>
                <ContextMenu.Item class="mi" onSelect={() => void copyPath(worktree.path)}>
                  <svg class="mi-ico" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3"
                    ><rect x="3" y="3" width="8" height="8" rx="1.5" /><path
                      d="M5.5 3V2.2a1 1 0 0 1 1-1H13a1 1 0 0 1 1 1v6.5a1 1 0 0 1-1 1h-.8"
                    /></svg
                  >
                  Copy path
                </ContextMenu.Item>
                <ContextMenu.Separator class="m-sep" />
                <ContextMenu.Item class="mi danger" onSelect={() => removeWorktreeTarget.set(worktree)}>
                  <svg class="mi-ico" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3"
                    ><path
                      d="M3 4.5h10M6 4.5V3.2a1 1 0 0 1 1-1h2a1 1 0 0 1 1 1V4.5M4.5 4.5l.6 8a1 1 0 0 0 1 1h3.8a1 1 0 0 0 1-1l.6-8"
                    /></svg
                  >
                  Remove worktree…
                </ContextMenu.Item>
              </ContextMenu.Content>
            </ContextMenu.Portal>
          </ContextMenu.Root>
        {/each}
      </div>
    {/if}
  {/each}
</div>

<style>
  .tree {
    padding: 8px 8px 4px;
  }
  .sec-head {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 9px 6px 6px;
    font-size: 10.5px;
    font-weight: 600;
    letter-spacing: 0.5px;
    text-transform: uppercase;
    color: var(--tx-lo);
  }
  .empty-copy {
    padding: 4px 8px 8px;
    font-size: 11.5px;
    color: var(--tx-lo);
    line-height: 1.5;
  }

  .row {
    display: flex;
    align-items: center;
    gap: 7px;
    width: 100%;
    /* Never let a long branch/project name push the icons or status word out of
       the rail — the flexible label is the only part that shrinks + ellipsizes. */
    min-width: 0;
    overflow: hidden;
    text-align: left;
    font: inherit;
    padding: 6px 8px;
    border-radius: var(--radius);
    color: var(--tx-md);
    cursor: pointer;
    border: 1px solid transparent;
    background: transparent;
    transition:
      background var(--t-fast),
      color var(--t-fast);
  }
  .row:hover {
    background: var(--bg-3);
    color: var(--tx-hi);
  }
  .row:focus-visible {
    outline: 2px solid var(--ac);
    outline-offset: -2px;
  }
  .row.sel {
    background: var(--ac-wash);
    color: var(--tx-hi);
  }
  .row.sel .lbl {
    font-weight: 560;
  }
  .row .twirl {
    width: 12px;
    height: 12px;
    color: var(--tx-lo);
    display: grid;
    place-items: center;
    padding: 0;
    border: 0;
    background: transparent;
    cursor: pointer;
    flex: none;
  }
  .row .twirl svg {
    width: 9px;
    height: 9px;
    transition: transform var(--t-fast);
  }
  .row .twirl.open svg {
    transform: rotate(90deg);
  }
  button.twirl:hover {
    color: var(--tx-hi);
  }
  .row .ico {
    width: 15px;
    height: 15px;
    color: var(--tx-lo);
    flex: none;
  }
  .row.sel .ico {
    color: var(--ac-bright);
  }
  .row .lbl {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .branch-main {
    flex: 1;
    min-width: 0;
    display: flex;
    align-items: baseline;
    gap: 7px;
  }
  .branch-main .lbl {
    flex: 0 1 auto;
  }
  .row .br-name {
    font-family: var(--mono);
    font-size: 11.5px;
  }
  .diffstat {
    display: inline-flex;
    gap: 4px;
    flex: none;
    font-family: var(--mono);
    font-size: 10.5px;
    font-weight: 600;
    white-space: nowrap;
  }
  .diffstat .add {
    color: var(--ok);
  }
  .diffstat .del {
    color: var(--err);
  }

  /* project rows read as group headers */
  .tree > .row .lbl {
    font-weight: 540;
  }
  .tree > .row {
    margin-top: 1px;
  }

  .worktrees {
    display: grid;
    gap: 2px;
  }
  /* tree hierarchy: one quiet vertical hairline under the caret, no ticks */
  .tree .worktrees {
    padding-left: 24px;
    margin: 2px 0 8px;
    position: relative;
  }
  .tree .worktrees::before {
    content: "";
    position: absolute;
    left: 14px;
    top: -1px;
    bottom: 12px;
    width: 1px;
    background: var(--line-soft);
  }
  .tree .wt-row {
    padding-left: 6px;
  }

  /* status as a WORD, in a reserved hue */
  .status {
    font-size: 10px;
    font-weight: 600;
    letter-spacing: 0.2px;
    flex: none;
    white-space: nowrap;
  }
  .status.run {
    color: var(--run);
  }
  .status.approval {
    color: var(--warn);
  }
  .status.done {
    color: var(--ok);
  }
  .status.error {
    color: var(--err);
  }
  .meta-r {
    display: flex;
    align-items: center;
    gap: 7px;
    flex: none;
  }

  /* per-project quick-add: a "+" on the project row that creates a worktree
     under it directly. Always visible, brightening on its own hover. */
  .row .quick-add {
    width: 18px;
    height: 18px;
    display: grid;
    place-items: center;
    padding: 0;
    border: 0;
    border-radius: 5px;
    background: transparent;
    color: var(--tx-lo);
    cursor: pointer;
    flex: none;
    transition:
      background var(--t-fast),
      color var(--t-fast);
  }
  .row .quick-add svg {
    width: 12px;
    height: 12px;
  }
  .row .quick-add:hover {
    background: var(--bg-4);
    color: var(--ac-bright);
  }
</style>
