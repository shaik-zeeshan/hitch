<script lang="ts">
  // Projects → Worktrees tree (mockup .tree). Project rows read as group
  // headers; their worktrees nest under an indent guide with connector ticks.
  // Agent state shows as a WORD in a reserved hue (rolled up to the project row
  // when collapsed). Dirty worktrees show their aggregate +/− line stat next
  // to the branch name.
  import { ContextMenu } from "bits-ui";
  import { invoke } from "@tauri-apps/api/core";
  import { revealItemInDir } from "@tauri-apps/plugin-opener";
  import { get } from "svelte/store";
  import { DEFAULT_EDITOR, editorApp } from "../settings";
  import {
    agentActRollupByProject,
    agentStateByWorktree,
    dirtyWorktrees,
    openSession,
    prByWorktree,
    projects,
    selectedProjectId,
    selectedWorktreeId,
    worktreeLineStats,
    worktrees,
  } from "../daemon";
  import { createWorktreeFor, removeProjectTarget, removeWorktreeTarget } from "../overlays";
  import {
    currentDesktopPlatform,
    revealItemLabel,
    shellSessionShortcutLabel,
  } from "../desktopPlatform";
  import { AGENT_LABEL, type Id, type PrInfo, type Project, type Worktree } from "../types";

  // The three kinds of worktree, distinguished only visually here — never
  // reordered (branches stay in daemon order). `main` is the repo's anchor and
  // is never removable; `managed` worktrees were created by Hitch and are safe
  // to remove destructively; `external` ones were discovered/imported, so Hitch
  // shows them but won't manage their lifecycle.
  type WorktreeKind = "main" | "managed" | "external";
  function worktreeKind(w: Worktree): WorktreeKind {
    if (w.is_main) return "main";
    return w.is_hitch_managed ? "managed" : "external";
  }
  const KIND_TITLE: Record<WorktreeKind, string> = {
    main: "Main worktree",
    managed: "Hitch-managed worktree",
    external: "External worktree (not managed by Hitch)",
  };

  // PR chip styling keys off draft first, then GitHub state. Colours are
  // GitHub-conventional (open=green, merged=purple, closed=red, draft=grey) and
  // deliberately distinct from the reserved agent-state hues — a `#`-prefixed
  // chip reads as a PR, not a status word.
  function prChipClass(pr: PrInfo): string {
    return pr.draft ? "draft" : pr.state.toLowerCase();
  }
  function prChipTitle(pr: PrInfo): string {
    const state = pr.draft ? "draft" : pr.state.toLowerCase();
    return `PR #${pr.number} (${state})`;
  }

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

  const desktopPlatform = currentDesktopPlatform();
  const revealMenuLabel = revealItemLabel(desktopPlatform);
  const shellShortcutLabel = shellSessionShortcutLabel(desktopPlatform);

  // Reveal the worktree in the OS file manager via the opener plugin.
  async function revealInFileManager(path: string) {
    try {
      await revealItemInDir(path);
    } catch (err) {
      // No file manager / denied — log and no-op rather than crash the menu.
      console.error(`${revealMenuLabel} failed:`, err);
    }
  }

  // Open the worktree in the configured editor via the backend so Windows can
  // resolve common editor install locations and pass spaced paths as one arg.
  async function openInEditor(path: string) {
    try {
      await invoke("open_in_editor", { path, editor: get(editorApp).trim() || DEFAULT_EDITOR });
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

  // Clicking a project row selects it (for palette context + the quick-add target)
  // and ensures its worktree list is visible. It MUST also clear the selected
  // worktree so even re-clicking the SAME project row returns the UI to the
  // project-level “choose a worktree” state instead of leaving the prior branch
  // selected.
  function selectProject(p: Project) {
    selectedProjectId.set(p.id);
    selectedWorktreeId.set(null);
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
    {@const rollup = $agentActRollupByProject[project.id]}
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
              <svg class="ico" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round"
                ><circle cx="4" cy="3.5" r="1.6" /><circle cx="4" cy="12.5" r="1.6" /><circle cx="12" cy="5" r="1.6" /><path
                  d="M4 5.1v5.8M12 6.6C12 9.8 8.8 11 4.6 11"
                /></svg
              >
            {:else}
              <svg class="ico" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3"
                ><path d="M1.5 4.5a2 2 0 0 1 2-2h3l1.5 1.6h4.5a2 2 0 0 1 2 2v5.4a2 2 0 0 1-2 2h-9a2 2 0 0 1-2-2z" /></svg
              >
            {/if}

            <span class="lbl">{project.name}</span>

            <span class="right">
              {#if !expanded && rollup}
                {@const label = AGENT_LABEL[rollup.state]}
                {#if label}
                  <span class="pill rollup {label.cls}">
                    {label.label}{#if rollup.count > 1}&nbsp;{rollup.count}{/if}
                  </span>
                {/if}
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
            </span>
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
          <ContextMenu.Item class="mi" onSelect={() => void revealInFileManager(project.root)}>
            <svg class="mi-ico" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3"
              ><path d="M1.5 4.5a2 2 0 0 1 2-2h3l1.5 1.6h4.5a2 2 0 0 1 2 2v5.4a2 2 0 0 1-2 2h-9a2 2 0 0 1-2-2z" /></svg
            >
            {revealMenuLabel}
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
          {@const isActive = worktree.id === $selectedWorktreeId}
          {@const hasLoc = !!lineStat && (lineStat.additions > 0 || lineStat.deletions > 0)}
          {@const kind = worktreeKind(worktree)}
          {@const pr = $prByWorktree[worktree.id]}
          <ContextMenu.Root>
            <ContextMenu.Trigger>
              {#snippet child({ props })}
                <button
                  {...props}
                  class="row wt-row {kind}"
                  class:sel={isActive}
                  onclick={() => selectWorktree(worktree)}
                >
                  <!-- A leading dot marks the worktree's kind: filled accent for
                       the repo's main anchor, solid for a Hitch-managed branch,
                       hollow for an external one Hitch only observes. -->
                  <span class="wt-dot {kind}" title={KIND_TITLE[kind]}></span>
                  <span class="lbl br-name">{worktree.branch}</span>
                  <!-- A fixed-order right-side cluster, each part shown only when
                       it applies, so a branch never has to choose between signals:
                       agent-state word · +/− line stat · PR chip. The agent word
                       is suppressed on the worktree you're IN — you can see that
                       agent live in the main pane — but its diff and PR still show. -->
                  <span class="right">
                    {#if !isActive && wtStatus}
                      {@const label = AGENT_LABEL[wtStatus]}
                      {#if label}<span class="pill {label.cls}">{label.label}</span>{/if}
                    {/if}
                    {#if hasLoc && lineStat}
                      <span
                        class="diffstat"
                        title={`${lineStat.additions} additions, ${lineStat.deletions} deletions`}
                      >
                        {#if lineStat.additions > 0}<span class="add">{lineStat.additions}+</span>{/if}
                        {#if lineStat.deletions > 0}<span class="del">{lineStat.deletions}-</span>{/if}
                      </span>
                    {/if}
                    {#if pr}
                      <span class="pr-chip {prChipClass(pr)}" title={prChipTitle(pr)}>#{pr.number}</span>
                    {/if}
                  </span>
                </button>
              {/snippet}
            </ContextMenu.Trigger>
            <ContextMenu.Portal>
              <ContextMenu.Content class="menu">
                <ContextMenu.Item class="mi" onSelect={() => launch(worktree, null, "shell")}>
                  <svg class="mi-ico" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3"
                    ><path d="M3 4l3.5 4L3 12M8 12h5" /></svg
                  >
                  Open shell session<span class="mi-k">{shellShortcutLabel}</span>
                </ContextMenu.Item>
                <ContextMenu.Item class="mi" onSelect={() => launch(worktree, ["claude"], "claude")}>
                  <span class="mi-ico" style="color:var(--warn); display:grid; place-items:center">✳</span>
                  Launch Claude
                </ContextMenu.Item>
                <ContextMenu.Separator class="m-sep" />
                <ContextMenu.Item class="mi" onSelect={() => void revealInFileManager(worktree.path)}>
                  <svg class="mi-ico" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3"
                    ><path d="M1.5 4.5a2 2 0 0 1 2-2h3l1.5 1.6h4.5a2 2 0 0 1 2 2v5.4a2 2 0 0 1-2 2h-9a2 2 0 0 1-2-2z" /></svg
                  >
                  {revealMenuLabel}
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
                {#if worktree.is_hitch_managed && !worktree.is_main}
                  <ContextMenu.Separator class="m-sep" />
                  <ContextMenu.Item class="mi danger" onSelect={() => removeWorktreeTarget.set(worktree)}>
                    <svg class="mi-ico" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3"
                      ><path
                        d="M3 4.5h10M6 4.5V3.2a1 1 0 0 1 1-1h2a1 1 0 0 1 1 1V4.5M4.5 4.5l.6 8a1 1 0 0 0 1 1h3.8a1 1 0 0 0 1-1l.6-8"
                      /></svg
                    >
                    Remove worktree…
                  </ContextMenu.Item>
                {/if}
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
    /* denser than the old 6px rows so more branches fit at a glance */
    padding: 4px 8px;
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
  .row .br-name {
    font-family: var(--mono);
    font-size: 11.5px;
  }
  /* the right-hand cluster: agent word · diffstat · PR chip, each optional */
  .right {
    display: flex;
    align-items: center;
    gap: 6px;
    flex: none;
  }

  /* leading kind marker on worktree rows */
  .wt-dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    flex: none;
    box-sizing: border-box;
    background: var(--tx-lo);
  }
  .wt-dot.main {
    background: var(--ac-bright);
  }
  .wt-dot.external {
    background: transparent;
    border: 1px solid var(--tx-lo);
  }
  /* an external worktree isn't ours to manage — read it back a notch */
  .wt-row.external .br-name {
    color: var(--tx-lo);
  }
  .wt-row.external:hover .br-name,
  .wt-row.external.sel .br-name {
    color: var(--tx-md);
  }

  /* PR chip: GitHub-conventional state colour, distinct from the agent hues */
  .pr-chip {
    flex: none;
    font-family: var(--mono);
    font-size: 9.5px;
    font-weight: 600;
    line-height: 1;
    padding: 2px 5px;
    border-radius: 5px;
    white-space: nowrap;
    border: 1px solid transparent;
  }
  .pr-chip.open {
    color: oklch(77% 0.13 150);
    background: oklch(77% 0.13 150 / 0.13);
  }
  .pr-chip.merged {
    color: oklch(72% 0.13 300);
    background: oklch(72% 0.13 300 / 0.15);
  }
  .pr-chip.closed {
    color: var(--err);
    background: oklch(68% 0.17 25 / 0.13);
  }
  /* a draft is open-but-not-ready: quiet, outlined, no fill */
  .pr-chip.draft {
    color: var(--tx-lo);
    border-color: var(--line-soft);
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
    gap: 1px;
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

  /* agent state as a human-language word inside a tinted pill, reserved hue */
  .pill {
    font-size: 9.5px;
    font-weight: 600;
    letter-spacing: 0.2px;
    flex: none;
    white-space: nowrap;
    padding: 1.5px 7px;
    border-radius: 999px;
  }
  .pill.run {
    color: var(--run);
    background: oklch(78% 0.1 195 / 0.15);
  }
  .pill.approval {
    color: var(--warn);
    background: oklch(81% 0.13 75 / 0.16);
  }
  .pill.wait {
    color: var(--ok);
    background: oklch(77% 0.12 150 / 0.14);
  }
  .pill.error {
    color: var(--err);
    background: oklch(68% 0.17 25 / 0.16);
  }

  /* per-project quick-add: a "+" on the project row that creates a worktree
     under it directly. Hidden at rest so the resting row stays quiet; revealed
     on row hover/focus. On a collapsed project it shares the slot with the
     rolled-up pill, which steps aside when the "+" appears. */
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
    opacity: 0;
    pointer-events: none;
    transition:
      background var(--t-fast),
      color var(--t-fast),
      opacity var(--t-fast);
  }
  .row .quick-add svg {
    width: 12px;
    height: 12px;
  }
  .row:hover .quick-add,
  .row:focus-within .quick-add {
    opacity: 1;
    pointer-events: auto;
  }
  .row .quick-add:hover {
    background: var(--bg-4);
    color: var(--ac-bright);
  }
  /* rolled-up pill yields to the "+" when the row is hovered/focused */
  .row:hover .pill.rollup,
  .row:focus-within .pill.rollup {
    display: none;
  }
</style>
