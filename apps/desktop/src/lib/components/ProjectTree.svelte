<script lang="ts">
  // Projects → Worktrees tree (mockup .tree). Project rows read as group
  // headers; their worktrees nest under an indent guide with connector ticks.
  // Agent state shows as a WORD in a reserved hue (rolled up to the project row
  // when collapsed). Dirty worktrees show their aggregate +/− line stat next
  // to the branch name, plus the iris dot as the compact dirty signal.
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
  import { createWorktreeFor, removeWorktreeTarget } from "../overlays";
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

  function selectProject(p: Project) {
    selectedProjectId.set(p.id);
    if (p.kind === "git-backed") collapsed = { ...collapsed, [p.id]: false };
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
    <div
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
          style={expanded ? "transform: rotate(90deg)" : ""}
          aria-label={expanded ? "Collapse" : "Expand"}
          onclick={(e) => {
            e.stopPropagation();
            toggleExpand(project);
          }}>▶</button
        >
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

      {#if project.kind === "plain"}
        <span class="tag">plain</span>
      {:else if expanded}
        <span class="tag">git</span>
      {:else if status}
        <span class="status {AGENT_LABEL[status].cls}">{AGENT_LABEL[status].label}</span>
      {:else}
        <span class="tag">git</span>
      {/if}
    </div>

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
                  {#if $dirtyWorktrees[worktree.id] || wtStatus}
                    <span class="meta-r">
                      {#if $dirtyWorktrees[worktree.id]}
                        <span class="dirtydot" title="uncommitted changes"></span>
                      {/if}
                      {#if wtStatus}
                        <span class="status {AGENT_LABEL[wtStatus].cls}">{AGENT_LABEL[wtStatus].label}</span>
                      {/if}
                    </span>
                  {/if}
                  {#if worktree.is_main}<span class="tag">main</span>{/if}
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

        <button class="row wt-row wt-add" onclick={() => createWorktreeFor.set(project)}>
          <span class="plus-ico">+</span>
          <span class="lbl">New worktree…</span>
        </button>
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
    box-shadow: inset 0 0 0 1px oklch(62% 0.1 265 / 0.28);
  }
  .row.sel .lbl {
    font-weight: 560;
  }
  .row .twirl {
    width: 12px;
    color: var(--tx-lo);
    font-size: 9px;
    display: grid;
    place-items: center;
    padding: 0;
    border: 0;
    background: transparent;
    cursor: pointer;
    transition: transform var(--t-fast);
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
  .row .tag {
    font-size: 9.5px;
    color: var(--tx-lo);
    font-family: var(--mono);
    padding: 1px 5px;
    border: 1px solid var(--line);
    border-radius: 4px;
    flex: none;
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
    gap: 1px;
  }
  /* tree hierarchy: indent guide + connector ticks */
  .tree .worktrees {
    padding-left: 16px;
    margin: 2px 0 9px;
    position: relative;
  }
  .tree .worktrees::before {
    content: "";
    position: absolute;
    left: 6px;
    top: -2px;
    bottom: 13px;
    width: 1px;
    background: var(--line-soft);
  }
  .tree .wt-row {
    padding-left: 12px;
    position: relative;
  }
  .tree .wt-row::before {
    content: "";
    position: absolute;
    left: -10px;
    top: 15px;
    width: 9px;
    height: 1px;
    background: var(--line-soft);
  }
  .tree .wt-row.wt-add::before {
    display: none;
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
  .dirtydot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--ac);
    flex: none;
  }
  .meta-r {
    display: flex;
    align-items: center;
    gap: 7px;
    flex: none;
  }

  .wt-add {
    color: var(--tx-lo);
  }
  .wt-add:hover {
    color: var(--ac-bright);
  }
  .wt-add .plus-ico {
    width: 12px;
    text-align: center;
    flex: none;
  }
</style>
